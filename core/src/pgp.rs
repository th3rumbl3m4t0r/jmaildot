//! OpenPGP with Sequoia: keys, decrypt + verify, encrypt + sign, detached signatures.
//! Passphrases are kept in memory (per key) once they were typed; the key ring is shared
//! by the sync engine (which decrypts what it downloads) and the editor.

use anyhow::{anyhow, bail, Result};
use sequoia_openpgp as openpgp;
use openpgp::cert::prelude::*;
use openpgp::crypto::{Password, SessionKey};
use openpgp::packet::{PKESK, SKESK};
use openpgp::parse::stream::*;
use openpgp::parse::Parse;
use openpgp::policy::{Policy, StandardPolicy};
use openpgp::serialize::stream::*;
use openpgp::serialize::SerializeInto;
use openpgp::types::{CompressionAlgorithm, HashAlgorithm, SymmetricAlgorithm};
use openpgp::{Cert, Fingerprint, KeyHandle};
use std::collections::HashMap;
use std::io::{Read, Write};

/// The secret key is there but locked: the passphrase is needed.
#[derive(Debug)]
pub struct NeedPassphrase;
impl std::fmt::Display for NeedPassphrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "your key is locked: the passphrase is needed")
    }
}
impl std::error::Error for NeedPassphrase {}

/// Encrypted to nobody we have a key for.
#[derive(Debug)]
pub struct NoKey;
impl std::fmt::Display for NoKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "encrypted to a key we don't have")
    }
}
impl std::error::Error for NoKey {}

pub fn needs_passphrase(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<NeedPassphrase>().is_some())
}

pub fn no_key(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<NoKey>().is_some())
}

#[derive(Clone, Debug)]
pub struct KeyInfo {
    pub fingerprint: String,
    pub user_ids: Vec<String>,
    pub emails: Vec<String>,
    pub created: i64,
    pub expires: Option<i64>,
    pub secret: bool,
    pub algorithm: String,
    pub can_encrypt: bool,
    pub can_sign: bool,
}

impl KeyInfo {
    pub fn pretty_fpr(&self) -> String {
        self.fingerprint.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
    }
    pub fn expired(&self) -> bool {
        self.expires.map(|e| e < crate::db::now()).unwrap_or(false)
    }
}

/// One signature on a message, checked against the keys we know.
#[derive(Clone, Debug)]
pub struct Sig {
    pub valid: bool,
    pub known: bool,
    pub fingerprint: Option<String>,
    pub key_id: String,
    pub user_ids: Vec<String>,
}

impl Sig {
    pub fn emails(&self) -> Vec<String> {
        self.user_ids.iter().filter_map(|u| email_of_uid(u)).collect()
    }
}

pub fn email_of_uid(u: &str) -> Option<String> {
    if let (Some(a), Some(b)) = (u.rfind('<'), u.rfind('>')) {
        if a < b {
            return Some(u[a + 1..b].trim().to_lowercase());
        }
    }
    if u.contains('@') {
        Some(u.trim().to_lowercase())
    } else {
        None
    }
}

pub struct Decrypted {
    pub data: Vec<u8>,
    pub encrypted: bool,
    pub sigs: Vec<Sig>,
}

/// The standard policy, plus GnuPG 2.4's AEAD (OCB) packets: gpg uses them for keys that
/// announce AEAD, which is every key it makes, so mail from gpg users needs them. We only
/// read them; what we write is the usual SEIPD.
fn policy() -> StandardPolicy<'static> {
    let mut p = StandardPolicy::new();
    p.accept_packet_tag(openpgp::packet::Tag::AED);
    p
}

/// Every key we hold (ours with their secrets, other people's public ones) and the
/// passphrases typed this session.
#[derive(Default, Clone)]
pub struct KeyRing {
    pub certs: Vec<Cert>,
    passwords: HashMap<Fingerprint, Password>,
}

pub fn info(c: &Cert) -> KeyInfo {
    let p = policy();
    let user_ids: Vec<String> = c.userids().map(|u| String::from_utf8_lossy(u.userid().value()).into_owned()).collect();
    let valid = c.with_policy(&p, None).ok();
    let expires = valid.as_ref().and_then(|v| v.primary_key().key_expiration_time()).map(|t| {
        t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    });
    let can_encrypt = c.keys().with_policy(&p, None).supported().alive().revoked(false).for_transport_encryption().next().is_some();
    let can_sign = c.keys().with_policy(&p, None).supported().alive().revoked(false).for_signing().next().is_some();
    KeyInfo {
        fingerprint: c.fingerprint().to_hex(),
        emails: user_ids.iter().filter_map(|u| email_of_uid(u)).collect(),
        user_ids,
        created: c.primary_key().creation_time().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
        expires,
        secret: c.is_tsk(),
        algorithm: format!("{}", c.primary_key().pk_algo()),
        can_encrypt,
        can_sign,
    }
}

/// Every certificate in an armored or binary blob (a key file may hold several).
pub fn parse_certs(data: &[u8]) -> Result<Vec<Cert>> {
    let mut out = Vec::new();
    for c in CertParser::from_bytes(data)? {
        out.push(c?);
    }
    if out.is_empty() {
        bail!("no OpenPGP key in that");
    }
    Ok(out)
}

pub fn armored(c: &Cert) -> Result<String> {
    let v = if c.is_tsk() { c.as_tsk().armored().to_vec()? } else { c.armored().to_vec()? };
    Ok(String::from_utf8(v)?)
}

pub fn public_armored(c: &Cert) -> Result<String> {
    Ok(String::from_utf8(c.clone().strip_secret_key_material().armored().to_vec()?)?)
}

fn secret_locked(c: &Cert) -> bool {
    c.keys().secret().any(|k| k.key().secret().is_encrypted())
}

impl KeyRing {
    pub fn new(certs: Vec<Cert>) -> KeyRing {
        KeyRing { certs, passwords: HashMap::new() }
    }

    pub fn secrets(&self) -> Vec<&Cert> {
        self.certs.iter().filter(|c| c.is_tsk()).collect()
    }

    /// Our own key: the first one with a secret.
    pub fn ours(&self) -> Option<&Cert> {
        self.certs.iter().find(|c| c.is_tsk())
    }

    /// A public key for [email] that can encrypt.
    pub fn for_email(&self, email: &str) -> Option<&Cert> {
        let e = email.trim().to_lowercase();
        let p = policy();
        self.certs.iter().find(|c| {
            info(c).emails.iter().any(|x| *x == e)
                && c.keys().with_policy(&p, None).supported().alive().revoked(false).for_transport_encryption().next().is_some()
        })
    }

    /// A secret key is locked and its passphrase isn't known yet.
    pub fn locked(&self) -> bool {
        self.secrets().iter().any(|c| secret_locked(c) && !self.passwords.contains_key(&c.fingerprint()))
    }

    pub fn has_password(&self, c: &Cert) -> bool {
        !secret_locked(c) || self.passwords.contains_key(&c.fingerprint())
    }

    /// Tries [pass] on every locked secret key; true when it opened at least one.
    pub fn unlock(&mut self, pass: &str) -> bool {
        let pw = Password::from(pass);
        let mut ok = false;
        for c in self.certs.iter().filter(|c| c.is_tsk()) {
            for k in c.keys().secret() {
                if k.key().secret().is_encrypted() && k.key().clone().decrypt_secret(&pw).is_ok() {
                    self.passwords.insert(c.fingerprint(), pw.clone());
                    ok = true;
                    break;
                }
            }
        }
        ok
    }

    /// Carries the passphrases typed this session over to a reloaded ring.
    pub fn keep_passwords(&mut self, old: &KeyRing) {
        for (k, v) in &old.passwords {
            self.passwords.insert(k.clone(), v.clone());
        }
    }

    pub fn forget_passwords(&mut self) {
        self.passwords.clear();
    }

    fn keypair_for_signing(&self, c: &Cert) -> Result<openpgp::crypto::KeyPair> {
        let p = policy();
        let ka = c
            .keys()
            .with_policy(&p, None)
            .supported()
            .alive()
            .revoked(false)
            .for_signing()
            .secret()
            .next()
            .ok_or_else(|| anyhow!("your key cannot sign"))?;
        let mut key = ka.key().clone();
        if key.secret().is_encrypted() {
            let pw = self.passwords.get(&c.fingerprint()).ok_or(NeedPassphrase)?;
            key = key.decrypt_secret(pw).map_err(|_| NeedPassphrase)?;
        }
        key.into_keypair()
    }

    /// Decrypts (and verifies what is signed inside) an OpenPGP message.
    pub fn decrypt(&self, data: &[u8]) -> Result<Decrypted> {
        let p = policy();
        let why = std::cell::Cell::new(0u8);
        let h = Helper { ring: self, sigs: Vec::new(), encrypted: false, policy: &p, need_pass: false, why: &why };
        // Sequoia doesn't hand the helper's error back, so the helper says why in [why]
        let explain = |e: anyhow::Error| -> anyhow::Error {
            match why.get() {
                1 => anyhow::Error::new(NeedPassphrase),
                2 => anyhow::Error::new(NoKey),
                _ => e,
            }
        };
        let mut d = DecryptorBuilder::from_bytes(data)?.with_policy(&p, None, h).map_err(explain)?;
        let mut out = Vec::new();
        d.read_to_end(&mut out).map_err(|e| explain(e.into()))?;
        let h = d.into_helper();
        Ok(Decrypted { data: out, encrypted: h.encrypted, sigs: h.sigs })
    }

    /// Checks a detached signature over [data].
    pub fn verify_detached(&self, data: &[u8], sig: &[u8]) -> Result<Vec<Sig>> {
        let p = policy();
        let why = std::cell::Cell::new(0u8);
        let h = Helper { ring: self, sigs: Vec::new(), encrypted: false, policy: &p, need_pass: false, why: &why };
        let mut v = DetachedVerifierBuilder::from_bytes(sig)?.with_policy(&p, None, h)?;
        v.verify_bytes(data)?;
        Ok(v.into_helper().sigs)
    }

    /// Encrypted to every cert in [to] (ours is added by the caller), signed by [signer] when given; armored.
    pub fn encrypt(&self, data: &[u8], to: &[&Cert], signer: Option<&Cert>) -> Result<Vec<u8>> {
        let p = policy();
        let mut recipients = Vec::new();
        for c in to {
            let mut n = 0;
            for ka in c.keys().with_policy(&p, None).supported().alive().revoked(false).for_transport_encryption().for_storage_encryption() {
                recipients.push(ka);
                n += 1;
            }
            if n == 0 {
                bail!("the key {} cannot encrypt", c.fingerprint());
            }
        }
        let keypair = match signer {
            Some(s) => Some(self.keypair_for_signing(s)?),
            None => None,
        };
        let mut sink = Vec::new();
        {
            let message = Message::new(&mut sink);
            let message = Armorer::new(message).build()?;
            let message = Encryptor2::for_recipients(message, recipients).symmetric_algo(SymmetricAlgorithm::AES256).build()?;
            let message = Compressor::new(message).algo(CompressionAlgorithm::Zip).build()?;
            let message = match keypair {
                Some(kp) => Signer::new(message, kp).hash_algo(HashAlgorithm::SHA256)?.build()?,
                None => message,
            };
            let mut w = LiteralWriter::new(message).build()?;
            w.write_all(data)?;
            w.finalize()?;
        }
        Ok(sink)
    }

    /// An armored detached signature over [data] (SHA-256).
    pub fn sign_detached(&self, data: &[u8], signer: &Cert) -> Result<Vec<u8>> {
        let kp = self.keypair_for_signing(signer)?;
        let mut sink = Vec::new();
        {
            let message = Message::new(&mut sink);
            let message = Armorer::new(message).kind(openpgp::armor::Kind::Signature).build()?;
            let mut s = Signer::new(message, kp).detached().hash_algo(HashAlgorithm::SHA256)?.build()?;
            s.write_all(data)?;
            s.finalize()?;
        }
        Ok(sink)
    }
}

struct Helper<'a> {
    ring: &'a KeyRing,
    sigs: Vec<Sig>,
    encrypted: bool,
    policy: &'a dyn Policy,
    need_pass: bool,
    /// 1: a key is locked, 2: no key for this message.
    why: &'a std::cell::Cell<u8>,
}

fn uids(c: &Cert) -> Vec<String> {
    c.userids().map(|u| String::from_utf8_lossy(u.userid().value()).into_owned()).collect()
}

impl<'a> VerificationHelper for Helper<'a> {
    fn get_certs(&mut self, _ids: &[KeyHandle]) -> openpgp::Result<Vec<Cert>> {
        Ok(self.ring.certs.clone())
    }

    fn check(&mut self, structure: MessageStructure) -> openpgp::Result<()> {
        for layer in structure.into_iter() {
            match layer {
                MessageLayer::Encryption { .. } => self.encrypted = true,
                MessageLayer::SignatureGroup { results } => {
                    for r in results {
                        let sig = match r {
                            Ok(GoodChecksum { ka, sig }) => Sig {
                                valid: true,
                                known: true,
                                fingerprint: Some(ka.cert().fingerprint().to_hex()),
                                key_id: sig.issuers().next().map(|i| i.to_hex()).unwrap_or_default(),
                                user_ids: uids(ka.cert()),
                            },
                            Err(VerificationError::MissingKey { sig }) => Sig {
                                valid: false,
                                known: false,
                                fingerprint: None,
                                key_id: sig.issuers().next().map(|i| i.to_hex()).unwrap_or_default(),
                                user_ids: vec![],
                            },
                            Err(VerificationError::BadSignature { sig, ka, .. }) => Sig {
                                valid: false,
                                known: true,
                                fingerprint: Some(ka.cert().fingerprint().to_hex()),
                                key_id: sig.issuers().next().map(|i| i.to_hex()).unwrap_or_default(),
                                user_ids: uids(ka.cert()),
                            },
                            Err(VerificationError::BadKey { sig, ka, .. }) => Sig {
                                valid: false,
                                known: true,
                                fingerprint: Some(ka.cert().fingerprint().to_hex()),
                                key_id: sig.issuers().next().map(|i| i.to_hex()).unwrap_or_default(),
                                user_ids: uids(ka.cert()),
                            },
                            Err(VerificationError::UnboundKey { sig, cert, .. }) => Sig {
                                valid: false,
                                known: true,
                                fingerprint: Some(cert.fingerprint().to_hex()),
                                key_id: sig.issuers().next().map(|i| i.to_hex()).unwrap_or_default(),
                                user_ids: uids(cert),
                            },
                            Err(VerificationError::MalformedSignature { sig, .. }) => Sig {
                                valid: false,
                                known: false,
                                fingerprint: None,
                                key_id: sig.issuers().next().map(|i| i.to_hex()).unwrap_or_default(),
                                user_ids: vec![],
                            },
                        };
                        self.sigs.push(sig);
                    }
                }
                MessageLayer::Compression { .. } => {}
            }
        }
        Ok(())
    }
}

impl<'a> DecryptionHelper for Helper<'a> {
    fn decrypt<D>(&mut self, pkesks: &[PKESK], _skesks: &[SKESK], sym_algo: Option<SymmetricAlgorithm>, mut decrypt: D) -> openpgp::Result<Option<Fingerprint>>
    where
        D: FnMut(SymmetricAlgorithm, &SessionKey) -> bool,
    {
        self.encrypted = true;
        let _ = self.policy;
        for pkesk in pkesks {
            for cert in self.ring.certs.iter().filter(|c| c.is_tsk()) {
                for ka in cert.keys().secret() {
                    let rid = pkesk.recipient();
                    if !rid.is_wildcard() && ka.key().keyid() != *rid {
                        continue;
                    }
                    let mut key = ka.key().clone();
                    if key.secret().is_encrypted() {
                        match self.ring.passwords.get(&cert.fingerprint()) {
                            Some(pw) => match key.decrypt_secret(pw) {
                                Ok(k) => key = k,
                                Err(_) => {
                                    self.need_pass = true;
                                    continue;
                                }
                            },
                            None => {
                                self.need_pass = true;
                                continue;
                            }
                        }
                    }
                    let mut kp = key.into_keypair()?;
                    if let Some((algo, sk)) = pkesk.decrypt(&mut kp, sym_algo) {
                        if decrypt(algo, &sk) {
                            return Ok(Some(cert.fingerprint()));
                        }
                    }
                }
            }
        }
        if self.need_pass {
            self.why.set(1);
            Err(anyhow::Error::new(NeedPassphrase))
        } else {
            self.why.set(2);
            Err(anyhow::Error::new(NoKey))
        }
    }
}

/// Generates nothing; kept for symmetry with the Android app where keys are imported only.
pub fn describe_sigs(sigs: &[Sig], sender: &str) -> String {
    let mut parts = Vec::new();
    for s in sigs {
        if !s.known {
            parts.push(format!("signed by an unknown key {}", short_id(&s.key_id)));
        } else if !s.valid {
            parts.push(format!("BAD signature by {}", s.user_ids.first().cloned().unwrap_or_else(|| short_id(s.fingerprint.as_deref().unwrap_or("")))));
        } else {
            let who = s.user_ids.first().cloned().unwrap_or_default();
            let matches = s.emails().iter().any(|e| e.eq_ignore_ascii_case(sender));
            if matches || sender.is_empty() {
                parts.push(format!("signed by {who}"));
            } else {
                parts.push(format!("signed by {who} — not the sender"));
            }
        }
    }
    parts.join(" · ")
}

fn short_id(s: &str) -> String {
    let t = s.trim();
    if t.len() > 16 {
        t[t.len() - 16..].to_string()
    } else {
        t.to_string()
    }
}
