#!/usr/bin/env python3
"""Talks to maildot's debug socket (MAILDOT_DEBUG_SOCK): each argument is one command;
'sleep N' pauses N seconds between commands. Prints the answers."""
import socket, sys, time

def main():
    s = socket.socket(socket.AF_UNIX)
    s.connect("/tmp/maildot-ui.sock")
    f = s.makefile("rw")
    for cmd in sys.argv[1:]:
        if cmd.startswith("sleep "):
            time.sleep(float(cmd.split()[1]))
            continue
        f.write(cmd + "\n")
        f.flush()
        out = []
        for line in f:
            if line == ".\n":
                break
            out.append(line)
        print("".join(out), end="")

main()
