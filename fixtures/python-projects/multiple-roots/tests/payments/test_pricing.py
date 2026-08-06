from payments import pricing


def test_total() -> None:
    assert pricing.calculate_total() == 42
