from api import checkout


def test_checkout_total() -> None:
    assert checkout.checkout_total() == 42
