MAX_SIZE = 3
default_name = "x"


def compute(x):
    return _clamp(x)


def _clamp(x):
    return min(x, MAX_SIZE)


class Engine:
    def run(self):
        self.step_once()

    def step_once(self):
        return compute(1)

    def drain_queue(self):
        return []
