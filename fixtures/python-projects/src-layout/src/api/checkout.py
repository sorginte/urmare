from payments.service import create_payment


def checkout() -> None:
    create_payment()
