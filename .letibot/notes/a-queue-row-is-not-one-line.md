<!-- abstract: `queue status` printed 98 lines for one repository, and one rejected review row's `evidence` can be 7 828 characters - so a bounded default with a word cap is the only shape that fits. -->

# A queue row is not one line, and `status` printed 98 of them

Three shapes in `mergequeue::desk`. `status` is the session's own repository, bounded: ten rows and `+N more`, a row's words cut at 200. `all: true` is one line per repository. `repository: "<path, tail or directory name>"` is the full shape, which is also what `/clear` prints. The `+N more` line points at `repository: "."`, which resolves against the session's workspace through git, so the model never has to spell a path.

The reason a row cap needs a character cap too: over 168 review rows the longest `failure` is 399 characters and the longest `evidence` is 7 828 - a rejected verdict's argument on one line, about 2 000 tokens for one row.

Rake: a count appended after a closing paren reads as a different number. `every repository (2)` plus `", 2 red"` is two counts on one line; build the header first and interpolate once.

Rake: the default must not be `None` when the scope cannot be resolved. `None` meant every repository, so a session seated above several repositories got the whole box. "I could not tell" is a third answer - say it in a sentence and name both escapes. And a folder above several repositories is `repos_of_workspace`, not `repo_of_workspace`.
