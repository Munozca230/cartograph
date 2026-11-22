class Base:
    def save(self):
        return self.validate()

    def validate(self):
        return True


class Mixin:
    def save(self):
        return None

    def log(self, msg):
        return msg


class Config:
    def load(self):
        return {}
