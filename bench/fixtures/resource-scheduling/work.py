import pathlib
import sys
import time

output = pathlib.Path(sys.argv[1])
milliseconds = int(sys.argv[2])
time.sleep(milliseconds / 1000)
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(pathlib.Path("salt.txt").read_text(encoding="utf-8"), encoding="utf-8")
