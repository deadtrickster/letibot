#!/usr/bin/env python3
"""R40: the bar's arithmetic, and the swap that pays for `/t`.

R22 measured this bar at 136 columns with the frame at 80 and concluded *a chord
appended to the end is a chord nobody has*. The bar is 151 now, so the first thing
to do is re-measure rather than quote.
"""

OLD = (
    "ctrl-s sessions · ctrl-n notes · ctrl-p todos · ctrl-g subagents · "
    "ctrl-r thinking · ctrl-t long output · ctrl-q jobs · "
    "tab completes /commands · /help"
)

# The entry that is WRONG: it says `long output`, and ctrl-t opens one row's rest.
WRONG = "ctrl-t long output"
# What ctrl-t does, and what `/t` does, as one adjacent pair — the pair is the point,
# because they are the two things a reader confuses.
PAIR = "ctrl-t newest result · /t all tool rows"
# What gives up its space, and why it is the one: see the report.
GIVE = "tab completes /commands"

NEW = OLD.replace(WRONG, PAIR).replace(" · " + GIVE, "")

print(f"old: {len(OLD)}")
print(f"new: {len(NEW)}")
print(f"  the pair costs      +{len(PAIR) - len(WRONG)}")
print(f"  {GIVE!r} yields  -{len(GIVE) + 3}")
print(f"  net                 {len(NEW) - len(OLD):+d}")
print()
for label, bar in (("OLD", OLD), ("NEW", NEW)):
    print(f"{label}: {bar}")
    print()
    pos = 0
    for item in bar.split(" · "):
        end = pos + len(item)
        edge = "visible at 80" if end <= 80 else "OFF at 80"
        print(f"  {pos:>3}..{end:<3} {edge:<12} {item}")
        pos = end + 3
    for width in (80, 100, 120, 210):
        cut = bar[:width]
        print(f"  at {width:>3}: {cut!r}")
    print()
