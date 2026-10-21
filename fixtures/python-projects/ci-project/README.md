Purpose-built CI fixture. `test_service.py` depends on `src/service.py` through
the `value` fixture in `conftest.py`; `test_unrelated.py` has no fixture request.
Tests of the CI consumer use a controlled subprocess harness, without importing
pytest or executing these files. Local Git repositories are created by the tests.
