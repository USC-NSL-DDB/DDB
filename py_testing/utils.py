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
            out = f"\n{meta} [ type: {type}, token: {token}, msg: {msg} ]\n{payload}\n" 
            if response["stream"] == "stdout":
                print(out, end="")
            if response["stream"] == "stderr":
                eprint(out, end="")
    except Exception as e:
        print(f"response: {response}. meta: {meta}, e: {e}")

def wrap_grouped_message(msg: str) -> str:
    return f"**** GROUPED RESPONSE START ****\n{msg}\n**** GROUPED RESPONSE END ****\n\n"

def parse_cmd(cmd: str) -> tuple:
    """
    Parses a gdb command string and returns a tuple containing the token, command without token,
    prefix, and the original command string.

    Args:
        cmd (str): The command string to be parsed.

    Returns:
        tuple: A tuple containing the token, command without token, prefix, and the original command string.
    """
    token = None
    cmd_no_token = None
    prefix = None
    cmd = cmd.strip()
    for idx, cmd_char in enumerate(cmd):
        if (not cmd_char.isdigit()) and (idx == 0):
            prefix = cmd.split()[0]
            cmd_no_token = cmd
            break
        
        if not cmd_char.isdigit():
            token = cmd[:idx].strip()
            cmd_no_token = cmd[idx:].strip()
            if len(cmd_no_token) == 0:
                # no meaningful input
                return (None, None, None)
            prefix = cmd_no_token.split()[0]
            break
    return (token, cmd_no_token, prefix, f"{cmd}\n")