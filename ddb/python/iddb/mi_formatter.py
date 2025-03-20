from enum import Enum
from typing import Optional
import json

# class MIResponseType(Enum):
#     Result = "^"

def escape_output(s: str) -> str:
    return json.dumps(str(s))

class MIFormatter:
    @staticmethod
    def format_list(payload: list) -> str:
        out_str = ""
        for i in payload:
            if isinstance(i, dict):
                out_str += "{" + MIFormatter.format_dict(i) + "}"
            elif isinstance(i, list):
                out_str += "[" + MIFormatter.format_list(i) + "]"
            else:
                out_str += escape_output(i)
            out_str += ","
        out_str = out_str.strip(",")
        return out_str

    @staticmethod
    def format_dict(payload: dict) -> str:
        out_str = ""
        for k, v in payload.items():
            out_str += f"{k}="
            if isinstance(v, dict):
                out_str +=  "{" + MIFormatter.format_dict(v) + "}"
            elif isinstance(v, list):
                out_str += "[" + MIFormatter.format_list(v) + "]"
            else:
                out_str += escape_output(v)
            out_str += ","
        out_str = out_str.strip(",")
        return out_str
                    

    @staticmethod
    def format(task_symbol: str, msg: Optional[str], payload: dict, token: Optional[str] = None) -> str:
        if not msg:
            print("No message to format")
            return 
        
        out_str = f"{token if token else ''}{task_symbol}{msg},{MIFormatter.format_dict(payload)}"
        return out_str
    @staticmethod
    def format_message(task_symbol: str, msg: Optional[str], token: Optional[str] = None) -> str:
        if not msg:
            print("No message to format")
            return 
        out_str = f"{token if token else ''}{task_symbol}{msg}"
        return out_str

if __name__ == "__main__":
    # Some tests 
    out_str = MIFormatter.format_dict(
        {
            "reason": "there should be some reason", 
            "frame": {
                "addr": "0x00007f8d6f6b6b7f",
                "func": "say_hello",
                "args": [
                    {
                        "name": "name",
                        "value": "John"
                    },
                    {
                        "name": "age",
                        "value": 30
                    }
                ]
            }, 
            "stopped-threads": [ "2", "3", "4" ]
        }
    )
    print(out_str)