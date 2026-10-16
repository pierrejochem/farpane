"""Hold a few RFB clients open on a server for a while, so the dashboard has
clients to show. Usage: hold.py PORT SECONDS [COUNT]"""
import socket
import struct
import sys
import time

port, seconds = int(sys.argv[1]), float(sys.argv[2])
count = int(sys.argv[3]) if len(sys.argv) > 3 else 3
socks = []
for _ in range(count):
    s = socket.create_connection(("127.0.0.1", port))
    s.recv(12)
    s.send(b"RFB 003.008\n")
    n = s.recv(1)[0]
    s.recv(n)
    s.send(b"\x01")
    s.recv(4)
    s.send(b"\x01")
    s.recv(100)
    s.send(struct.pack(">BxHi", 2, 1, 16))  # SetEncodings: ZRLE
    socks.append(s)
end = time.time() + seconds
while time.time() < end:
    for s in socks:
        s.settimeout(0.05)
        try:
            s.recv(1 << 20)
        except Exception:
            pass
        try:
            s.send(struct.pack(">BBHHHH", 3, 1, 0, 0, 1024, 768))
        except Exception:
            pass
