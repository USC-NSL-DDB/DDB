import sys

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
            out = f"{meta} [ type: {type}, token: {token}, msg: {msg} ] \n\t {payload}\n" 
            if response["stream"] == "stdout":
                print(out, end="")
            if response["stream"] == "stderr":
                eprint(out, end="")
    except Exception as e:
        print(f"response: {response}. meta: {meta}, e: {e}")
