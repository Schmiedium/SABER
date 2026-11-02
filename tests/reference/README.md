# Reference store

Reference cases for verification and validation, one directory per case, small files only
(`verification-and-test-strategy.md` §9.2):

```text
tests/reference/<source>/<case>/
  case.ron            # RefCase: scenario, what is compared, tolerance, provenance, tol_derivation
  reference.parquet   # the reference data, when the oracle is not a registered function
```

**CI data rule (TD1):** only synthetic data and published test data live here (NASA TM-2015-218675,
JSBSim, RotorPy, USSA 1976, UIUC propeller data, McCoy, Stevens–Lewis–Johnson F-16 tables, and other
published books and papers). Campaign data and anything unpublished stay outside the repository and
outside CI. `scripts/check_refcase_provenance.py` rejects a case without `provenance` or with a tolerance
but no `tol_derivation`.
