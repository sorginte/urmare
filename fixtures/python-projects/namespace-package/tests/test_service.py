from company.api import service


def test_service():
    assert service.VALUE == "core"
