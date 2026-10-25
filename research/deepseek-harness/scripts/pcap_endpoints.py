#!/usr/bin/env python3
"""List DNS query names and non-private TCP/UDP destinations from a tcpdump -i any pcap (Linux cooked v1/v2)."""
import struct, sys, collections, ipaddress

def main(path):
    d = open(path, "rb").read()
    magic = d[:4]
    le = magic in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1")
    e = "<" if le else ">"
    link = struct.unpack(e + "I", d[20:24])[0]
    off = 24; names = collections.Counter(); dests = collections.Counter()
    while off + 16 <= len(d):
        _s, _u, cl, _ol = struct.unpack(e + "IIII", d[off:off+16]); off += 16
        p = d[off:off+cl]; off += cl
        if link == 113: p = p[16:]; proto = None
        elif link == 276: p = p[20:]
        else: continue
        if not p or p[0] >> 4 != 4: continue
        ihl = (p[0] & 15) * 4; pr = p[9]
        src = str(ipaddress.ip_address(p[12:16])); dst = str(ipaddress.ip_address(p[16:20]))
        t = p[ihl:]
        if pr == 17 and len(t) > 8:
            dp = struct.unpack(">H", t[2:4])[0]; sp = struct.unpack(">H", t[0:2])[0]
            payload = t[8:]
            if len(payload) > 12 and (dp == 53 or dst.startswith("127.0.0.11") or sp == 53):
                q = struct.unpack(">H", payload[4:6])[0]
                if q and not (payload[2] & 0x80):
                    i = 12; labels = []
                    while i < len(payload) and payload[i]:
                        n = payload[i]; labels.append(payload[i+1:i+1+n].decode("ascii", "replace")); i += n + 1
                    qt = struct.unpack(">H", payload[i+1:i+3])[0]
                    names[(".".join(labels), {1: "A", 28: "AAAA", 65: "HTTPS"}.get(qt, str(qt)))] += 1
        if pr in (6, 17) and len(t) >= 4:
            dp = struct.unpack(">H", t[2:4])[0]
            if dp != 53 and not dst.startswith("127."): dests[(pr == 6 and "tcp" or "udp", dst, dp)] += 1
    print("DNS queries (name, type, count):")
    for (n, t), c in sorted(names.items()): print(f"  {n} {t} x{c}")
    print("destinations (proto, ip, port, packets):")
    for k, c in sorted(dests.items()): print("  ", *k, f"x{c}")

main(sys.argv[1])
