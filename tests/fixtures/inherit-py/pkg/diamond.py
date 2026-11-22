class Root:
    def hook(self):
        return 0


class Left(Root):
    pass


class Right(Root):
    def hook(self):
        return 1


class Diamond(Left, Right):
    def run(self):
        return self.hook()
