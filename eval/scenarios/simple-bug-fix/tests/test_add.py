import os
import sys

# Cwd-independent: anchor at this file's directory so the import works
# whether the agent runs `python tests/test_add.py` from the workspace
# root or `python test_add.py` from inside tests/.
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

from add import add

assert add(2, 3) == 5, f"add(2, 3) = {add(2, 3)}, expected 5"
assert add(10, 20) == 30, f"add(10, 20) = {add(10, 20)}, expected 30"
print("OK")
