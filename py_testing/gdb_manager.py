from pygdbmi.gdbcontroller import GdbController
from pprint import pprint

BIN_PATH = "../bin/hello_world"

class GdbManager:
    def __init__(self) -> None:
        self.sessions = []
        self.sessions.append(GdbController(command=[BIN_PATH]))
