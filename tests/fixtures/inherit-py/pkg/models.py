import requests

from .base import Base, Config, Mixin


class Model(Mixin, Base):
    def run(self):
        self.save()
        self.validate()
        super().save()
        super(Model, self).validate()
        self.log("ran")


class Child(Model):
    def go(self):
        self.validate()
        Child.log(self, "class call")


class Remote(requests.Session):
    def go(self):
        self.validate()


class Config(Config):
    def refresh(self):
        return self.load()
