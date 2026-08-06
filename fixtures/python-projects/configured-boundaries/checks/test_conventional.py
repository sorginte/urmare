"""Convention-based test outside the configured test root."""

from app import core


def test_core():
    assert core.VALUE == 1
