<!-- abstract: A gate-only test failure reproduces by moving `TMPDIR` to a real disk rather than by adding CPU load, and the reds it produces are fixtures asserting wall clocks that measure the box instead of the product. -->

# A gate red reproduced by moving `TMPDIR`, not by adding load

The `-p letibot-harnessd --lib` pair that failed 2 of 950 in the gate's whole-workspace run did not
reproduce under CPU load (48 hogs at load 32: passed) but reproduced 5 of 6 times with six copies of the
binary and `TMPDIR=/var/tmp/...` - the gate's own scratch, a real disk, against `/tmp`, which is a tmpfs
here. The binary took 115-136 s instead of 15 s.

Both failures were false statements about the product. A 3 500 ms "floor" in `leases_seen_over` fired
between the lease being claimed and the worker writing its health row, so a supervisor doing exactly
right was asserted against. And `serve_until`'s 20 s was a clock the outer 120 s ceiling could never
reach, which is why raising the ceiling changed nothing. A third shape from the same run: a fixture
asserting `deadline_ms - gate::now_ms()` in a range measures how long the FIXTURE has been running - the
row's own stamps (`deadline_ms - state_ms`) are the fact.

The rake: a wall clock in a fixture is a bet on the box, and the box it is bet on is the gate's. Only a hang detector far above any slow box
and a fact read off the product's own stamps are legitimate. To reproduce one, change `TMPDIR`; do not
add CPU load.
