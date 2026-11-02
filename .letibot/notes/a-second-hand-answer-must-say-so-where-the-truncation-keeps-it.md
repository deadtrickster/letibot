<!-- abstract: A recordless child's class has to travel on the row (`ChildName.lost`), not be a bool, and the caveat that its last row is not a finished answer must lead the payload. -->

# A second-hand answer must say so, and the caveat belongs at the head of the payload

`subagent_record::ended_in` said exactly where a recordless child stopped, and only one door printed it: the list row carried a bool (`ChildName.lost`) and one fixed sentence for every child that did not end on an assistant row, so a parent could not tell a child that died mid-`bash` from one that never opened. The class now travels on the row (`ChildName.lost: Option<LostChild>`, `Answered` or `Stopped(<clause>)`), built by `subagent_record::how_far` over the predicate that decides a text IS an answer.

The class must be asked by handle. `ChildName.lost` comes from `children_without_runs`, which is the children a record was DUE for; `TaskRunner::lost(handle)` answers for any child, which is what the answer door asks.

A child with no record's last `Assistant` row comes back as `Done { answer: text }`, and that is not an answer: during a shutdown an interrupted turn comes back as one. The caveat is `task::NOT_A_FINISHED_ANSWER`, used by both doors, and it leads the payload because `Spiller::apply` keeps the head half and a settlement notice is `first_line(&answer)`.

Rake: a bool that marks a row second-hand is not the class, and a sentence a reader must not miss goes first in the payload.
