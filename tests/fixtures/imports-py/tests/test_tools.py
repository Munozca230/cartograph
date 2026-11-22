# Top-level folders without __init__.py do not shadow installed packages (PEP 420).
from alembic import op
import docker.errors
# ...but a namespace package is local when the import lands on a file in it.
from plugins import extra
