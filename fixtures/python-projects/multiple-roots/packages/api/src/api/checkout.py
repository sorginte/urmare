from payments import pricing


def checkout_total() -> int:
    return pricing.calculate_total()
