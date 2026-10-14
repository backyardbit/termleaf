import fcntl, struct, sys, termios
r, c, x, y = struct.unpack("HHHH", fcntl.ioctl(1, termios.TIOCGWINSZ, b"\0" * 8))
open(sys.argv[1], "w").write(f"{c} {r} {x} {y} {x / max(c, 1)} {y / max(r, 1)}\n")
