from dataclasses import dataclass, field 

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
