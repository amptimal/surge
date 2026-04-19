# SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0
"""GO Competition Challenge 3 benchmark harness.

Pairs with :mod:`markets.go_c3` to add:

* Dataset discovery / unpacking (:mod:`.datasets`, :mod:`.manifests`).
* Validator integration — downloads & runs the official GO C3
  validator (:mod:`.validator`).
* Winner / leaderboard reference data (:mod:`.references`,
  :mod:`.leaderboard`).
* Suite runners and wrappers that add scoring artifacts
  (:mod:`.runner`).
* Reference-schedule extraction for SCED-fixed probes
  (:mod:`.commitment`).
* Result comparisons and diagnostic tools (:mod:`.compare`,
  :mod:`.comparator`, :mod:`.detail_compare`, :mod:`.q_term_diff`,
  :mod:`.ledger`, :mod:`.inspector`, :mod:`.winner_roundtrip`).
* The pi-model violation replica (:mod:`.violations`).
"""
