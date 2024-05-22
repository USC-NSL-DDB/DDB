from dataclasses import dataclass, field
from enum import Enum
from typing import List, Optional 
from ddb.gdbserver_starter import RemoteServerConnection, SSHRemoteServerClient

class SessionResponse:
    def __init__(self, sid: int, meta: str, response: dict) -> None:
        self.sid = sid
        self.meta = meta
        self.response = response
        self.token: str = None
        self.stream: str = response["stream"]
        self.payload: dict = response["payload"]
        self.msg: str = response["message"]

        if ("token" in response) and response["token"]:
            self.token = str(response["token"])

    def __str__(self) -> str:
        return f"Response - sid: {self.sid}, payload:\n\t{self.payload}"

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
    remote_gdbserver: RemoteServerConnection = None
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
    gdb_config_cmds: List[str] = field(default_factory=list)

@dataclass
class BrokerInfo:
    hostname: str
    port: int

@dataclass
class DDBConfig:
    gdb_sessions_configs: List[GdbSessionConfig] = field(default_factory=list)
    broker: BrokerInfo = None
