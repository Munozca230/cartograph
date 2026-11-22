import os

from flask import Flask

import pkg.core as core
from pkg import compute
from pkg.core import Engine

app = Flask(__name__)


@app.get("/items/<item_id>")
def item(item_id):
    engine = Engine()
    engine.drain_queue()
    engine.run()
    core.compute(2)
    os.path.join("a", "b")
    return compute(item_id)


def main():
    app.run()


if __name__ == "__main__":
    main()
