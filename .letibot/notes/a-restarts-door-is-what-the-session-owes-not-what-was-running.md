<!-- abstract: A restart's door is `restart::Owed` - a turn in flight, an undelivered settlement, a due row, a plan with rows owed - not "was anything running"; the mid-turn arm excludes `User` rows whose speaker is `Agent`. -->

# A restart's door is what the session OWES, not what was running

`restore_after_restart` (`crates/harnessd/src/harness/turns.rs`) has two doors, and the choice used to be *was anything running*. It is now *does the session owe anything*: a restart that restored work keeps its turn, and an otherwise empty one is a turn when `restart::Owed` holds. Nothing owed keeps the transcript row and spends no call.

`Owed` is four facts, none of them prose: `mid_turn`, `settlements`, `due_rows`, `plan_rows`. The mid-turn arm is the session row's `last_item` - an `Assistant` with no calls is a finished turn, and everything else is a turn that never got its answer, except a `User` row whose speaker is `Agent`. That exclusion is load-bearing: the empty restart's own row is one, and so is a wake's notice, so counting them makes an idle session alternate a model call into every second restart.

The row-not-a-turn door is `Harness::say_without_a_turn`: a turn in flight gets the sentence in `injected`, an idle session gets a `Speaker::Agent` row and no wake. Never append a `User` row mid-turn; it can land between a `tool_use` and its results.

Rake: a cost argument decides a door for the case it was measured on, not for the class it was written over.
