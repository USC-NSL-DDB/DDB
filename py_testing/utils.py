import sys
from threading import Lock

from counter import TSCounter

def eprint(*args, **kwargs):
    print(*args, file=sys.stderr, **kwargs)

def mi_print(response, meta: str):
    try:
        token = None
        if "token" in response:
            token = response["token"]

        type = response["type"]
        if type in [ "console", "output", "notify", "result" ]:
            msg = response["message"]
            payload = response["payload"] 
            out = f"\n{meta} [ type: {type}, token: {token}, message: {msg} ]\n{payload}\n" 
            if response["stream"] == "stdout":
                print(out, end="")
            if response["stream"] == "stderr":
                eprint(out, end="")
    except Exception as e:
        print(f"response: {response}. meta: {meta}, e: {e}")

def wrap_grouped_message(msg: str) -> str:
    return f"**** GROUPED RESPONSE START ****\n{msg}\n**** GROUPED RESPONSE END ****\n\n"
class CmdTokenGenerator:
    _sc: "CmdTokenGenerator" = None
    _lock = Lock()

    def __init__(self) -> None:
        self.counter = TSCounter()

    @staticmethod
    def inst() -> "CmdTokenGenerator":
        with CmdTokenGenerator._lock:
            if CmdTokenGenerator._sc:
                return CmdTokenGenerator._sc
            CmdTokenGenerator._sc = CmdTokenGenerator()
            return CmdTokenGenerator._sc

    def inc(self) -> int:
        return self.counter.increment()

    @staticmethod
    def get() -> int:
        return str(CmdTokenGenerator.inst().inc())