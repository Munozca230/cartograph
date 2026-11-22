import pytest
from fxpy.core import run
from fxpy.nothere import x


def test_run():
    assert run()
