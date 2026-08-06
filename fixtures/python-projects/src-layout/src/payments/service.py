from . import stripe


def create_payment() -> None:
    stripe.charge()
