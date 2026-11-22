import importlib
from ..models import User
from . import text, nothing_here


def load(name):
    return importlib.import_module(name), importlib.import_module("fxpy.utils.text"), User
