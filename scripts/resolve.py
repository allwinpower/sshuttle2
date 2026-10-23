"""Resolves a name with the system resolver; prints the addresses or the error."""
import socket
import sys

try:
    print(sorted({a[4][0] for a in socket.getaddrinfo(sys.argv[1], 80)}))
except Exception as e:
    print("failed: %s" % e)
