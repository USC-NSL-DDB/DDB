from dataclasses import dataclass, field
from enum import Enum
from typing import Dict, List, Optional 
from pprint import pformat
import getpass

from iddb.gdb_controller import RemoteGdbController

class SessionResponse:
    def __init__(self, sid: int, meta: str, response: dict) -> None:
        self.sid = sid
        self.meta = meta
        self.response = response
        self.token: str = None
        self.stream: str = response.get("stream", "unknown")
        self.payload: dict = response["payload"]
        self.msg: str = response["message"]

        if ("token" in response) and response["token"]:
            self.token = str(response["token"])

    def __str__(self) -> str:
        return f"Response - sid: {self.sid}, payload:\n\t{self.payload}"

    # def __repr__(self):
    #     return pformat(self.__dict__)

@dataclass
class ServiceInfo:
    ip: str           # ip address of the service as human-readable string. for example, "10.10.1.2"
    tag: str = ""
    pid: int = -1

class GdbMode(Enum):
    LOCAL = 1
    REMOTE = 2

class StartMode(Enum):
    BINARY = 1
    ATTACH = 2

@dataclass
class GdbSessionConfig:
    remote_port: int = -1
    remote_host: str = ""
    username: str = "" 
    # remote_gdbserver: RemoteServerConnection = None
    gdb_controller:RemoteGdbController = None
    attach_pid: int = -1
    binary: str = ""
    tag: Optional[str] = None
    gdb_mode: GdbMode = 1
    start_mode: StartMode = 1
    mi_version: str = "mi"
    cwd: str = "."
    # Using default_factory for mutable default
    args: List[str] = field(default_factory=list)
    run_delay: int = 0
    sudo: bool = False
    prerun_cmds:List[Dict[str,str]] = field(default_factory=list)
    initialize_commands:List[str] = field(default_factory=list)

    def __repr__(self):
        formatted_dict = pformat(self.__dict__)
        indented_lines = "\n".join("\t" + line for line in formatted_dict.splitlines())
        return f"{self.__class__.__name__}(\n{indented_lines}\n)"

@dataclass
class BrokerInfo:
    hostname: str
    port: int

@dataclass
class SSHInfo:
    port: int = 22
    user: str = getpass.getuser()

class TargetFramework(Enum):
    UNSPECIFIED = 1
    NU = 2
    SERVICE_WEAVER_K8S = 3

@dataclass
class DDBConfig:
    framework = TargetFramework.UNSPECIFIED
    gdb_sessions_configs: List[GdbSessionConfig] = field(default_factory=list)
    broker: BrokerInfo = None
    ssh: SSHInfo = SSHInfo()

    def __repr__(self):
        formatted_dict = pformat(self.__dict__)
        indented_lines = "\n".join("\t" + line for line in formatted_dict.splitlines())
        return f"{self.__class__.__name__}(\n{indented_lines}\n)"
