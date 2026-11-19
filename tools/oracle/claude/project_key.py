"""Reference implementation of Claude Code 2.1.328's projects/<key> folder name.

Usage: python3 -I project_key.py <absolute-path>
JS source (binary 2.1.328): w(e)=e.replace(/[^a-zA-Z0-9]/g,"-"); oD(e)= len<=200 ? w(e) : w(e).slice(0,200)+"-"+ge(e)
ge(e)=Math.abs(Ote(e)).toString(36); Ote: h=(h<<5)-h+charCodeAt(i)|0 over UTF-16 code units.
"""
import struct
import sys


def utf16_units(s):
    b = s.encode("utf-16-le", "surrogatepass")
    return struct.unpack("<%dH" % (len(b) // 2), b)


def base36(n):
    digits = "0123456789abcdefghijklmnopqrstuvwxyz"
    if n == 0:
        return "0"
    out = ""
    while n:
        n, r = divmod(n, 36)
        out = digits[r] + out
    return out


def project_key(path):
    units = utf16_units(path)
    sanitized = "".join(chr(u) if (48 <= u <= 57 or 65 <= u <= 90 or 97 <= u <= 122) else "-" for u in units)
    if len(sanitized) <= 200:
        return sanitized
    h = 0
    for u in units:
        h = ((h << 5) - h + u) & 0xFFFFFFFF
    if h >= 2 ** 31:
        h -= 2 ** 32
    return sanitized[:200] + "-" + base36(abs(h))


if __name__ == "__main__":
    print(project_key(sys.argv[1]))
