"""An independent RA2 implementation written from rfbproto.rst (RSA-AES Security
Type) to cross-check the Rust one. Usage: ra2py.py server PORT | client HOST PORT USER PASS"""
import hashlib, os, socket, struct, sys
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import rsa, padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.cmac import CMAC

def omac(key, t, data):
    c = CMAC(algorithms.AES(key)); c.update(bytes(15) + bytes([t]) + data); return c.finalize()

def ctr(key, iv, data):
    e = Cipher(algorithms.AES(key), modes.CTR(iv)).encryptor(); return e.update(data) + e.finalize()

def eax_encrypt(key, nonce, aad, msg):
    n = omac(key, 0, nonce); h = omac(key, 1, aad)
    c = ctr(key, n, msg); cc = omac(key, 2, c)
    tag = bytes(a ^ b ^ d for a, b, d in zip(n, cc, h))
    return c + tag

def eax_decrypt(key, nonce, aad, data):
    c, tag = data[:-16], data[-16:]
    n = omac(key, 0, nonce); h = omac(key, 1, aad); cc = omac(key, 2, c)
    want = bytes(a ^ b ^ d for a, b, d in zip(n, cc, h))
    assert want == tag, "EAX tag mismatch"
    return ctr(key, n, c)

def recvn(s, n):
    b = b""
    while len(b) < n:
        d = s.recv(n - len(b))
        if not d: raise EOFError
        b += d
    return b

class Layer:
    def __init__(self, s, send_key, recv_key):
        self.s, self.sk, self.rk, self.sc, self.rc = s, send_key, recv_key, 0, 0
    def send(self, plain):
        hdr = struct.pack(">H", len(plain))
        nonce = self.sc.to_bytes(16, "little"); self.sc += 1
        self.s.sendall(hdr + eax_encrypt(self.sk, nonce, hdr, plain))
    def recv(self):
        hdr = recvn(self.s, 2); n = struct.unpack(">H", hdr)[0]
        body = recvn(self.s, n + 16)
        nonce = self.rc.to_bytes(16, "little"); self.rc += 1
        return eax_decrypt(self.rk, nonce, hdr, body)

def pub_bytes(key):
    nums = key.public_key().public_numbers(); ln = (key.key_size + 7) // 8
    return struct.pack(">I", key.key_size) + nums.n.to_bytes(ln, "big") + nums.e.to_bytes(ln, "big")

def parse_pub(blob_reader):
    bits = struct.unpack(">I", blob_reader(4))[0]; ln = (bits + 7) // 8
    raw = blob_reader(2 * ln)
    n, e = int.from_bytes(raw[:ln], "big"), int.from_bytes(raw[ln:], "big")
    return struct.pack(">I", bits) + raw, rsa.RSAPublicNumbers(e, n).public_key()

def derive(h, srv_rand, cli_rand, klen):
    return h(srv_rand + cli_rand).digest()[:klen], h(cli_rand + srv_rand).digest()[:klen]

def server(port, variant=256):
    h, klen = (hashlib.sha256, 32) if variant == 256 else (hashlib.sha1, 16)
    ls = socket.socket(); ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    key = rsa.generate_private_key(65537, 2048); spub = pub_bytes(key)
    print("fingerprint", ":".join("%02X" % b for b in hashlib.sha256(spub).digest()), flush=True)
    ls.bind(("127.0.0.1", port)); ls.listen(1); print("listening", flush=True)
    s, _ = ls.accept()
    s.sendall(b"RFB 003.008\n"); recvn(s, 12)
    s.sendall(bytes([1, 129 if variant == 256 else 5])); assert recvn(s, 1)[0] == (129 if variant == 256 else 5)
    s.sendall(spub)
    cpub, ckey = parse_pub(lambda n: recvn(s, n))
    srv_rand = os.urandom(16)
    ct = ckey.encrypt(srv_rand, padding.PKCS1v15()); s.sendall(struct.pack(">H", len(ct)) + ct)
    ln = struct.unpack(">H", recvn(s, 2))[0]; cli_rand = key.decrypt(recvn(s, ln), padding.PKCS1v15())
    ck, sk = derive(h, srv_rand, cli_rand, klen)
    layer = Layer(s, sk, ck)
    layer.send(h(spub + cpub).digest())
    assert layer.recv() == h(cpub + spub).digest(), "client hash"
    layer.send(b"\x01")
    cred = layer.recv(); ul = cred[0]; user = cred[1:1+ul]; pl = cred[1+ul]; pw = cred[2+ul:2+ul+pl]
    print("credentials", user, pw, flush=True)
    ok = user == b"alice" and pw == b"s3cret"
    layer.send(struct.pack(">I", 0 if ok else 1))
    if not ok: return
    layer.recv()  # ClientInit
    pf = struct.pack(">BBBBHHHBBB3x", 32, 24, 0, 1, 255, 255, 255, 16, 8, 0)
    name = b"python ra2"
    layer.send(struct.pack(">HH", 64, 48) + pf + struct.pack(">I", len(name)) + name)
    print("serverinit sent", flush=True)
    try:
        while True: print("client message", layer.recv()[:12].hex(), flush=True)
    except Exception as e:
        print("end", repr(e))

def client(host, port, user, pw, variant=256):
    h, klen = (hashlib.sha256, 32) if variant == 256 else (hashlib.sha1, 16)
    s = socket.create_connection((host, port))
    recvn(s, 12); s.sendall(b"RFB 003.008\n")
    n = recvn(s, 1)[0]; types = list(recvn(s, n)); print("types", types)
    t = 129 if variant == 256 else 5; assert t in types; s.sendall(bytes([t]))
    spub, skey = parse_pub(lambda n: recvn(s, n))
    print("server key fingerprint", hashlib.sha256(spub).hexdigest())
    key = rsa.generate_private_key(65537, 2048); cpub = pub_bytes(key); s.sendall(cpub)
    ln = struct.unpack(">H", recvn(s, 2))[0]; srv_rand = key.decrypt(recvn(s, ln), padding.PKCS1v15())
    cli_rand = os.urandom(16); ct = skey.encrypt(cli_rand, padding.PKCS1v15()); s.sendall(struct.pack(">H", len(ct)) + ct)
    ck, sk = derive(h, srv_rand, cli_rand, klen)
    layer = Layer(s, ck, sk)
    assert layer.recv() == h(spub + cpub).digest(), "server hash"
    layer.send(h(cpub + spub).digest())
    sub = layer.recv(); print("subtype", sub)
    u, p = user.encode(), pw.encode()
    layer.send(bytes([len(u)]) + u + bytes([len(p)]) + p)
    res = layer.recv(); print("security result", res.hex())
    if res == b"\0\0\0\0":
        layer.send(b"\x01")  # ClientInit shared
        init = layer.recv(); print("serverinit", init[:4].hex(), init[20:].decode(errors="replace"))

if __name__ == "__main__":
    if sys.argv[1] == "server": server(int(sys.argv[2]), int(sys.argv[3]) if len(sys.argv) > 3 else 256)
    else: client(sys.argv[2], int(sys.argv[3]), sys.argv[4], sys.argv[5], int(sys.argv[6]) if len(sys.argv) > 6 else 256)
