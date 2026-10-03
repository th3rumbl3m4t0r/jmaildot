%global debug_package %{nil}

Name:           maildot
Version:        %{ver}
Release:        1%{?dist}
Summary:        mail. - mail and calendar for the desktop (JMAP, CalDAV, Sieve, OpenPGP)
License:        GPL-3.0-or-later
URL:            https://github.com/th3rumbl3m4t0r/jmaildot
Source0:        maildot-%{version}.tar.gz
ExclusiveArch:  x86_64

Requires:       gtk4 >= 4.16
Requires:       gtksourceview5
Requires:       libsecret
Requires:       dejavu-sans-mono-fonts

%description
Mail and calendar for the desktop, made for Stalwart: mail over JMAP (push and a light
poll), every message downloaded and kept locally as a decrypted .eml with an SQLite index,
searched on all cores (plain text or regex, case, whole words, fields, NOT). OpenPGP
(Sequoia), Sieve filters over JMAP with a rule builder, replies from catch-all aliases,
several accounts, calendars over CalDAV with sharing (shared calendars in panes of their
own), reminders, and shelf for attachments.

%prep
%setup -q

%install
install -Dm755 maildot %{buildroot}%{_bindir}/maildot
install -Dm644 io.github.th3rumbl3m4t0r.jmaildot.desktop %{buildroot}%{_datadir}/applications/io.github.th3rumbl3m4t0r.jmaildot.desktop
install -Dm644 io.github.th3rumbl3m4t0r.jmaildot.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.th3rumbl3m4t0r.jmaildot.svg
install -Dm644 io.github.th3rumbl3m4t0r.jmaildot.metainfo.xml %{buildroot}%{_metainfodir}/io.github.th3rumbl3m4t0r.jmaildot.metainfo.xml

%files
%{_bindir}/maildot
%{_datadir}/applications/io.github.th3rumbl3m4t0r.jmaildot.desktop
%{_datadir}/icons/hicolor/scalable/apps/io.github.th3rumbl3m4t0r.jmaildot.svg
%{_metainfodir}/io.github.th3rumbl3m4t0r.jmaildot.metainfo.xml

%changelog
* Sat Oct 03 2026 th3rumbl3m4t0r <th3rumbl3m4t0r@users.noreply.github.com> - 0.1.1-1
- the reader no longer offers "always for <domain>"; domains are added in settings -> html mail
- favourite from addresses on top of the from list (star / unstar there)
- outlines: pop-up windows and popovers in the accent (the border colour when not in front)

* Sat Oct 03 2026 th3rumbl3m4t0r <th3rumbl3m4t0r@users.noreply.github.com> - 0.1.0-1
- first release
