#!/usr/bin/env python3
"""Writes random regular expressions and whether Python's re.compile
accepts them, for the src/pyre.rs test. One case per line: 1 or 0, a tab,
the pattern as UTF-8 hex.

    python3 -I tools/gen-pyre-corpus.py [COUNT] [SEED] > src/testdata/pyre_corpus.txt
"""
import random
import re
import sys
import warnings

warnings.simplefilter("ignore")
count = int(sys.argv[1]) if len(sys.argv) > 1 else 3000
rng = random.Random(int(sys.argv[2]) if len(sys.argv) > 2 else 1)

# pieces weighted toward regex syntax, so most patterns hit the parser's
# edge cases rather than being plain literals
PIECES = (
    list("ab1()[]{}|*+?.^$-,:=!<>#\\ \n") + ["\\", "\\", "(", ")", "[", "]"]
    + ["(?", "(?P<", "(?P=", "(?:", "(?#", "(?=", "(?!", "(?<=", "(?<!", "(?(",
       "(?>", "(?i", "(?x)", "(?a", "(?u", "(?L", "(?-", "(?s:", "\\d", "\\b", "\\B",
       "\\A", "\\Z", "\\z", "\\x4", "\\x41", "\\u00e9", "\\U0001F600", "\\U00110000",
       "\\N{", "\\0", "\\1", "\\2", "\\12", "\\400", "\\377", "\\q", "\\-", "{2}",
       "{2,}", "{,3}", "{3,2}", "{1,2}?", "*?", "*+", "a-z", "z-a", "[^", "\\w-",
       "g>", "1)", "é", "_x", "4294967295"]
)

seen = set()
lines = []
while len(lines) < count:
    pattern = "".join(rng.choice(PIECES) for _ in range(rng.randint(1, 8)))
    if pattern in seen:
        continue
    seen.add(pattern)
    try:
        re.compile(pattern)
        ok = 1
    except Exception:
        ok = 0
    lines.append(f"{ok}\t{pattern.encode().hex()}")
print("\n".join(lines))
