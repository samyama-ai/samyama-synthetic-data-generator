# Where these files come from, and the notice they must carry

`nodes.jsonl` and `edges.jsonl` in this directory are a **derived form of NORIA-O 0.3**,
built from `noria.ttl` and `noria-kos.ttl` in `common_ws/samyama-ontologies/data/noria-o-0.3/`.
They are not our work: they are that ontology's classes and its `ProbableCause` scheme,
flattened for the generator's binding step.

NORIA-O is copyright 2022-2023 Orange under the four-clause BSD licence. Clauses 1 and 2
require a redistribution in source or derived form to retain the copyright notice, the list
of conditions and the disclaimer, so `LICENSE.txt` beside this file is the licence as
published and must travel with these two files. Clause 3 requires advertising material that
mentions features or use of the software to carry Orange's acknowledgement sentence, which
is given in that licence; clause 4 forbids using Orange's name to endorse or promote
derived work.

This notice was added on 2026-10-21. Before that, these derived files were redistributed
with no notice at all, which clauses 1 and 2 do not permit. The repository has been private
throughout, so nothing was published in breach, but the omission was real and the fix is
this file plus `LICENSE.txt`.

The open question of whether the telecom dataset itself may ship bound to NORIA-O is a
separate matter, reviewed in `telecom-kg/LICENCE-REVIEW-NORIA-O.md`. The dataset does not
redistribute these files.
