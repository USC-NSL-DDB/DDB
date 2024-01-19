import sys

def eprint(*args, **kwargs):
    print(*args, file=sys.stderr, **kwargs)

def mi_print(response, meta: str):
    try:
        if response["type"] in [ "console", "output", "notify" ]:
            payload = response["payload"] 
            out = f"{meta}\n\t {payload}" 
            if response["stream"] == "stdout":
                print(out, end="")
            if response["stream"] == "stderr":
                eprint(out, end="")
    except Exception as e:
        print(f"response: {response}. meta: {meta}, e: {e}")
