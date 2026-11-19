"""Usage: python3 -I dump.py <literal> [length=6000] [occurrence=0]  -- print bytes after a literal in the binary."""
import mmap, sys
BIN="/Users/sungjin/.local/share/claude/versions/2.1.328"
f=open(BIN,"rb"); mm=mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ)
pat=sys.argv[1].encode(); n=int(sys.argv[2]) if len(sys.argv)>2 else 6000; occ=int(sys.argv[3]) if len(sys.argv)>3 else 0
i=-1
for _ in range(occ+1):
    i=mm.find(pat,i+1)
    if i<0: sys.exit("not found")
print(f"@{i}: "+"".join(chr(b) if 32<=b<127 else "." for b in mm[i:i+n]))
