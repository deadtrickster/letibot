#!/bin/sh
# Walk up from this process to init, printing pid/comm, to see who spawned whom.
p=$$
i=0
while [ "$p" != "1" ] && [ -n "$p" ] && [ "$i" -lt 10 ]; do
  comm=$(cat "/proc/$p/comm" 2>/dev/null)
  ppid=$(awk '/^PPid:/{print $2}' "/proc/$p/status" 2>/dev/null)
  pcomm=$(cat "/proc/$ppid/comm" 2>/dev/null)
  echo "pid $p ($comm) -> parent $ppid ($pcomm)"
  p=$ppid
  i=$((i + 1))
done
