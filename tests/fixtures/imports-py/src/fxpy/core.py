import os
import json as j
from typing import Any
import requests
from fxpy.utils import helpers
from .models import User
from ..outside import nope
from fxpy import ns
import fxpy.ns


def run() -> Any:
    import numpy as np
    from fxpy import VERSION
    return np, VERSION, os, j, requests, helpers, User


try:
    import ujson as fastjson
except ImportError:
    import json as fastjson
