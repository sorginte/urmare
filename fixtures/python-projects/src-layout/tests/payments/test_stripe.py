from payments import stripe


def test_charge() -> None:
    stripe.charge()
