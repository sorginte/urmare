"""Unconventionally named test selected through test-roots."""

from app import service


def check_checkout():
    assert service.VALUE == 1
