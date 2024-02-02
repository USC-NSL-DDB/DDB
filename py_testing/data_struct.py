class SessionResponse:
    def __init__(self, sid: int, meta: str, response: dict) -> None:
        self.sid = sid
        self.meta = meta
        self.response = response
        self.token: str = None
        self.stream: str = response["stream"]
        self.payload: str = response["payload"]
        self.msg: str = response["message"]

        if ("token" in response) and response["token"]:
            self.token = str(response["token"])

    def __str__(self) -> str:
        return f"Response - sid: {self.sid}, payload:\n\t{self.payload}"
