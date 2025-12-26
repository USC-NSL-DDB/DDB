from typing import Dict, List, Optional, Set, Iterator, TYPE_CHECKING, Union, Any
from enum import Enum
import socket
import time
import struct
import platform
import inspect
import fnmatch
import re

import gdb  # type: ignore
import gdb.FrameDecorator # type: ignore

if TYPE_CHECKING:
    from gdb.FrameDecorator import FrameDecorator as _FrameDecorator, DAPFrameDecorator as _DAPFrameDecorator  # type: ignore
    _GdbFrameDecorator = Union[_FrameDecorator, _DAPFrameDecorator]
else:
    _GdbFrameDecorator = Any

# try:
#     import debugpy
# except ImportError:
#     print("Failed to import debugpy")

# try:
#     import random
#     # port = random.randint(5700, 5800)
#     port = 5800
#     debugpy.listen(("localhost", port))
#     print(f"Waiting for debugger attach: {port}")
#     debugpy.wait_for_client()
# except Exception as e:
#     print(f"Failed to attach debugger: {e}")

class LogLevel(Enum):
    ERROR = "ERROR"
    WARNING = "WARNING"
    INFO = "INFO"
    DEBUG = "DEBUG"
    TRACE = "TRACE"


def log_level_to_int(level: LogLevel) -> int:
    level_map = {
        LogLevel.ERROR: 40,
        LogLevel.WARNING: 30,
        LogLevel.INFO: 20,
        LogLevel.DEBUG: 10,
        LogLevel.TRACE: 5,
    }
    return level_map.get(level, 0)


G_LOG_LEVEL = LogLevel.INFO

def dbg(level: LogLevel, *args):
    curr_frame = inspect.currentframe()
    if log_level_to_int(level) < log_level_to_int(G_LOG_LEVEL):
        return
    out_str = f"[({level.value}) "
    if curr_frame is not None:
        frame = curr_frame.f_back
        if frame is not None:
            filename = frame.f_code.co_filename
            lineno = frame.f_lineno
            funcname = frame.f_code.co_name
            out_str += f"{filename}:{lineno} in {funcname}] "
            for arg in args:
                out_str += f"{arg!r} "
            out_str += "\n"
            gdb.write(out_str)
            return
    else:
        out_str += "unknown location] "
        for arg in args:
            out_str += f"{arg!r} "
        out_str += "\n"
        gdb.write(out_str)


# ============================================================================
# Frame Filter Implementation
# ============================================================================


class MatchType(Enum):
    """Pattern matching types for filter rules"""
    EXACT = "exact"
    GLOB = "glob"
    REGEX = "regex"


class FilterRule:
    """A single filter rule with pattern and match type"""
    
    def __init__(self, pattern: str, match_type: MatchType):
        self.pattern = pattern
        self.match_type = match_type
        # Pre-compile regex for performance
        if match_type == MatchType.REGEX:
            try:
                self._compiled = re.compile(pattern)
            except re.error as e:
                dbg(LogLevel.ERROR, f"Invalid regex pattern '{pattern}': {e}")
                self._compiled = None
        else:
            self._compiled = None
    
    def matches(self, value: str) -> bool:
        """Check if value matches this rule's pattern"""
        if not value:
            return False
        
        if self.match_type == MatchType.EXACT:
            return value == self.pattern
        elif self.match_type == MatchType.GLOB:
            return fnmatch.fnmatch(value, self.pattern)
        elif self.match_type == MatchType.REGEX:
            if self._compiled:
                return bool(self._compiled.match(value))
            return False
        return False
    
    def to_dict(self) -> Dict[str, str]:
        """Convert to dictionary for serialization"""
        return {
            "pattern": self.pattern,
            "match_type": self.match_type.value
        }


class FilterConfig:
    """Configuration for frame filtering"""
    
    def __init__(self):
        self.enabled = False  # Filter starts disabled
        self.mode = "blacklist"  # or "whitelist"
        self.function_rules: List[FilterRule] = []
        self.file_rules: List[FilterRule] = []
        self.active_presets: Set[str] = set()
    
    def add_function_rule(self, pattern: str, match_type: MatchType):
        """Add a function filter rule"""
        # Avoid duplicates
        if not any(r.pattern == pattern and r.match_type == match_type 
                   for r in self.function_rules):
            self.function_rules.append(FilterRule(pattern, match_type))
            dbg(LogLevel.DEBUG, f"Added function rule: {pattern} ({match_type.value})")
    
    def remove_function_rule(self, pattern: str):
        """Remove function filter rule(s) by pattern"""
        original_len = len(self.function_rules)
        self.function_rules = [r for r in self.function_rules if r.pattern != pattern]
        removed = original_len - len(self.function_rules)
        dbg(LogLevel.DEBUG, f"Removed {removed} function rule(s) matching: {pattern}")
    
    def add_file_rule(self, pattern: str, match_type: MatchType):
        """Add a file filter rule"""
        # Avoid duplicates
        if not any(r.pattern == pattern and r.match_type == match_type 
                   for r in self.file_rules):
            self.file_rules.append(FilterRule(pattern, match_type))
            dbg(LogLevel.DEBUG, f"Added file rule: {pattern} ({match_type.value})")
    
    def remove_file_rule(self, pattern: str):
        """Remove file filter rule(s) by pattern"""
        original_len = len(self.file_rules)
        self.file_rules = [r for r in self.file_rules if r.pattern != pattern]
        removed = original_len - len(self.file_rules)
        dbg(LogLevel.DEBUG, f"Removed {removed} file rule(s) matching: {pattern}")
    
    def enable_preset(self, preset_name: str) -> bool:
        """Enable a framework preset"""
        if preset_name not in FRAMEWORK_PRESETS:
            dbg(LogLevel.WARNING, f"Unknown preset: {preset_name}")
            return False
        
        preset = FRAMEWORK_PRESETS[preset_name]
        
        # Add function patterns
        for func_pattern in preset.get("functions", []):
            self.add_function_rule(func_pattern, MatchType.GLOB)
        
        # Add file patterns
        for file_pattern in preset.get("files", []):
            self.add_file_rule(file_pattern, MatchType.GLOB)
        
        self.active_presets.add(preset_name)
        dbg(LogLevel.INFO, f"Enabled preset: {preset_name}")
        return True
    
    def disable_preset(self, preset_name: str) -> bool:
        """Disable a framework preset"""
        if preset_name not in self.active_presets:
            dbg(LogLevel.WARNING, f"Preset not active: {preset_name}")
            return False
        
        preset = FRAMEWORK_PRESETS[preset_name]
        
        # Remove function patterns
        for func_pattern in preset.get("functions", []):
            self.remove_function_rule(func_pattern)
        
        # Remove file patterns
        for file_pattern in preset.get("files", []):
            self.remove_file_rule(file_pattern)
        
        self.active_presets.remove(preset_name)
        dbg(LogLevel.INFO, f"Disabled preset: {preset_name}")
        return True
    
    def should_filter_frame(self, func_name: Optional[str], file_name: Optional[str]) -> bool:
        """
        Determine if a frame should be filtered out.
        Returns True if the frame should be hidden.
        """
        if not self.enabled:
            return False
        
        matched = False
        
        # Check function rules
        if func_name:
            for rule in self.function_rules:
                if rule.matches(func_name):
                    matched = True
                    dbg(LogLevel.TRACE, f"Function '{func_name}' matched rule: {rule.pattern}")
                    break
        
        # Check file rules (if not already matched)
        if not matched and file_name:
            for rule in self.file_rules:
                if rule.matches(file_name):
                    matched = True
                    dbg(LogLevel.TRACE, f"File '{file_name}' matched rule: {rule.pattern}")
                    break
        
        # Apply mode logic
        if self.mode == "blacklist":
            return matched  # Filter out if matched
        else:  # whitelist
            return not matched  # Filter out if NOT matched
    
    def to_dict(self) -> Dict[str, object]:
        """Convert configuration to dictionary"""
        return {
            "enabled": self.enabled,
            "mode": self.mode,
            "function_rules": [r.to_dict() for r in self.function_rules],
            "file_rules": [r.to_dict() for r in self.file_rules],
            "active_presets": list(self.active_presets)
        }


# Framework-specific filter presets
FRAMEWORK_PRESETS = {
    "ddb-runtime": {
        "functions": ["DDB::*"],
        "files": [],
        "description": "DDB runtime backtrace extraction frames"
    },
    "serviceweaver": {
        "functions": ["*runHandler", "github.com/ServiceWeaver/*"],
        "files": [],
        "description": "Service Weaver runtime frames"
    },
    "go-runtime": {
        "functions": ["runtime.*", "runtime/internal/*"],
        "files": [],
        "description": "Go runtime internal frames"
    },
    "cpp-stdlib": {
        "functions": ["std::*", "__gnu_cxx::*", "__cxx*"],
        "files": ["/usr/include/*", "/usr/lib/*"],
        "description": "C++ standard library frames"
    },
    "networking": {
        "functions": ["net.*", "net::*"],
        "files": [],
        "description": "Network library frames"
    },
    "grpc": {
        "functions": ["grpc::*", "grpc_*", "google::protobuf::*"],
        "files": [
            "*/grpc/*",
            "*/grpcpp/*",
            "*/google/protobuf/*",
            "/usr/include/grpc*/*",
            "/usr/local/include/grpc*/*"
        ],
        "description": "gRPC and Protocol Buffers internal frames"
    },
    "protobuf-gen": {
        "functions": [],
        "files": [
            "*.grpc.pb.cc"
        ],
        "description": "Protobuf generated codes"
    }
}


class DDBFrameFilter:
    """Main frame filter for DDB"""
    
    def __init__(self):
        self.name = "DDBFrameFilter"
        self.priority = 100  # Higher priority filters run first
        self.enabled = True  # The filter itself is enabled; config controls filtering
        self.config = FilterConfig()
        dbg(LogLevel.DEBUG, "DDBFrameFilter initialized")
    
    def filter(self, frame_iter):
        """
        Filter the frame iterator.
        This is the main entry point called by GDB.
        
        The key insight: we should only YIELD frames we want to keep.
        Don't wrap frames - just check and yield or skip.
        """
        dbg(LogLevel.TRACE, "DDBFrameFilter.filter() invoked")
        
        if not self.config.enabled:
            # If filtering is disabled, yield all frames unchanged
            for frame in frame_iter:
                yield frame
            return
        
        # Check each frame and only yield the ones we want to keep
        for frame in frame_iter:
            func_name = None
            file_name = None
            
            try:
                func = frame.function()
                if func:
                    func_name = str(func)
            except Exception as e:
                dbg(LogLevel.TRACE, f"Error getting function name: {e}")
            
            try:
                file = frame.filename()
                if file:
                    file_name = str(file)
            except Exception as e:
                dbg(LogLevel.TRACE, f"Error getting filename: {e}")
            
            # Check if frame should be filtered
            should_hide = self.config.should_filter_frame(func_name, file_name)
            
            if should_hide:
                dbg(LogLevel.DEBUG, f"Filtering out frame: func={func_name}, file={file_name}")
                # Don't yield this frame - it will be hidden
            else:
                # Yield the frame unchanged
                yield frame


class DDBFilterCommand(gdb.Command):
    """Configure DDB frame filter
    
    Usage:
        ddb-filter enable|disable              - Enable/disable filtering
        ddb-filter status                      - Show current configuration
        ddb-filter add-function <pattern> [--type exact|glob|regex]
        ddb-filter remove-function <pattern>
        ddb-filter add-file <pattern> [--type exact|glob|regex]
        ddb-filter remove-file <pattern>
        ddb-filter preset list                 - List available presets
        ddb-filter preset enable <name>        - Enable a preset
        ddb-filter preset disable <name>       - Disable a preset
        ddb-filter clear                       - Clear all filter rules
        ddb-filter mode blacklist|whitelist    - Set filter mode
    
    Examples:
        ddb-filter enable
        ddb-filter add-function "std::*"
        ddb-filter preset enable go-runtime
        ddb-filter mode blacklist
    """
    
    def __init__(self, filter_obj):
        super(DDBFilterCommand, self).__init__("ddb-filter", gdb.COMMAND_STACK)
        self.filter = filter_obj
    
    def invoke(self, argument, from_tty):
        args = gdb.string_to_argv(argument)
        
        if not args:
            gdb.write("Usage: ddb-filter <subcommand> [args]\n")
            gdb.write("Use 'help ddb-filter' for more information\n")
            return
        
        cmd = args[0]
        
        try:
            if cmd == "enable":
                self.filter.config.enabled = True
                gdb.write("DDB frame filter enabled\n")
            
            elif cmd == "disable":
                self.filter.config.enabled = False
                gdb.write("DDB frame filter disabled\n")
            
            elif cmd == "status":
                self._print_status()
            
            elif cmd == "add-function":
                if len(args) < 2:
                    gdb.write("Error: Missing pattern\n")
                    gdb.write("Usage: ddb-filter add-function <pattern> [--type exact|glob|regex]\n")
                    return
                pattern = args[1]
                match_type = self._parse_match_type(args)
                self.filter.config.add_function_rule(pattern, match_type)
                gdb.write(f"Added function filter: {pattern} ({match_type.value})\n")
            
            elif cmd == "remove-function":
                if len(args) < 2:
                    gdb.write("Error: Missing pattern\n")
                    gdb.write("Usage: ddb-filter remove-function <pattern>\n")
                    return
                pattern = args[1]
                self.filter.config.remove_function_rule(pattern)
                gdb.write(f"Removed function filter: {pattern}\n")
            
            elif cmd == "add-file":
                if len(args) < 2:
                    gdb.write("Error: Missing pattern\n")
                    gdb.write("Usage: ddb-filter add-file <pattern> [--type exact|glob|regex]\n")
                    return
                pattern = args[1]
                match_type = self._parse_match_type(args)
                self.filter.config.add_file_rule(pattern, match_type)
                gdb.write(f"Added file filter: {pattern} ({match_type.value})\n")
            
            elif cmd == "remove-file":
                if len(args) < 2:
                    gdb.write("Error: Missing pattern\n")
                    gdb.write("Usage: ddb-filter remove-file <pattern>\n")
                    return
                pattern = args[1]
                self.filter.config.remove_file_rule(pattern)
                gdb.write(f"Removed file filter: {pattern}\n")
            
            elif cmd == "preset":
                self._handle_preset(args[1:])
            
            elif cmd == "clear":
                self.filter.config.function_rules = []
                self.filter.config.file_rules = []
                self.filter.config.active_presets = set()
                gdb.write("Cleared all filter rules\n")
            
            elif cmd == "mode":
                if len(args) < 2:
                    gdb.write(f"Current mode: {self.filter.config.mode}\n")
                    gdb.write("Usage: ddb-filter mode blacklist|whitelist\n")
                elif args[1] in ["blacklist", "whitelist"]:
                    self.filter.config.mode = args[1]
                    gdb.write(f"Filter mode set to: {args[1]}\n")
                else:
                    gdb.write("Error: Mode must be 'blacklist' or 'whitelist'\n")
            
            else:
                gdb.write(f"Unknown subcommand: {cmd}\n")
                gdb.write("Use 'help ddb-filter' for usage information\n")
        
        except Exception as e:
            gdb.write(f"Error: {e}\n")
            dbg(LogLevel.ERROR, f"DDBFilterCommand error: {e}")
    
    def _parse_match_type(self, args: List[str]) -> MatchType:
        """Parse --type flag from arguments"""
        try:
            type_idx = args.index("--type")
            if type_idx + 1 < len(args):
                type_str = args[type_idx + 1]
                return MatchType(type_str)
        except (ValueError, KeyError):
            pass
        return MatchType.GLOB  # Default to glob matching
    
    def _handle_preset(self, args: List[str]):
        """Handle preset subcommands"""
        if not args:
            gdb.write("Usage: ddb-filter preset list|enable|disable <name>\n")
            return
        
        subcmd = args[0]
        
        if subcmd == "list":
            gdb.write("Available presets:\n")
            for name, preset in FRAMEWORK_PRESETS.items():
                status = " (active)" if name in self.filter.config.active_presets else ""
                gdb.write(f"  {name}: {preset['description']}{status}\n")
                if preset.get("functions"):
                    gdb.write(f"    Functions: {', '.join(preset['functions'])}\n")
                if preset.get("files"):
                    gdb.write(f"    Files: {', '.join(preset['files'])}\n")
        
        elif subcmd == "enable":
            if len(args) < 2:
                gdb.write("Error: Missing preset name\n")
                gdb.write("Usage: ddb-filter preset enable <name>\n")
                gdb.write("Use 'ddb-filter preset list' to see available presets\n")
                return
            preset_name = args[1]
            if self.filter.config.enable_preset(preset_name):
                gdb.write(f"Enabled preset: {preset_name}\n")
            else:
                gdb.write(f"Unknown preset: {preset_name}\n")
                gdb.write("Use 'ddb-filter preset list' to see available presets\n")
        
        elif subcmd == "disable":
            if len(args) < 2:
                gdb.write("Error: Missing preset name\n")
                gdb.write("Usage: ddb-filter preset disable <name>\n")
                return
            preset_name = args[1]
            if self.filter.config.disable_preset(preset_name):
                gdb.write(f"Disabled preset: {preset_name}\n")
            else:
                gdb.write(f"Preset not active: {preset_name}\n")
        
        else:
            gdb.write(f"Unknown preset subcommand: {subcmd}\n")
            gdb.write("Usage: ddb-filter preset list|enable|disable <name>\n")
    
    def _print_status(self):
        """Print current filter configuration"""
        config = self.filter.config
        gdb.write("DDB Frame Filter Status:\n")
        gdb.write("=" * 50 + "\n")
        gdb.write(f"  Enabled: {config.enabled}\n")
        gdb.write(f"  Mode: {config.mode}\n")
        
        gdb.write(f"\nActive Presets ({len(config.active_presets)}):\n")
        if config.active_presets:
            for preset in sorted(config.active_presets):
                gdb.write(f"  - {preset}\n")
        else:
            gdb.write("  (none)\n")
        
        gdb.write(f"\nFunction Rules ({len(config.function_rules)}):\n")
        if config.function_rules:
            for rule in config.function_rules:
                gdb.write(f"  - {rule.pattern} ({rule.match_type.value})\n")
        else:
            gdb.write("  (none)\n")
        
        gdb.write(f"\nFile Rules ({len(config.file_rules)}):\n")
        if config.file_rules:
            for rule in config.file_rules:
                gdb.write(f"  - {rule.pattern} ({rule.match_type.value})\n")
        else:
            gdb.write("  (none)\n")


class DDBFilterMICommand(gdb.MICommand):
    """MI interface for DDB frame filter configuration
    
    Usage:
        -ddb-filter-config --enable
        -ddb-filter-config --disable
        -ddb-filter-config --add-function <pattern> --match-type <type>
        -ddb-filter-config --remove-function <pattern>
        -ddb-filter-config --add-file <pattern> --match-type <type>
        -ddb-filter-config --remove-file <pattern>
        -ddb-filter-config --preset-enable <name>
        -ddb-filter-config --preset-disable <name>
        -ddb-filter-config --list-presets
        -ddb-filter-config --status
        -ddb-filter-config --clear
        -ddb-filter-config --mode <blacklist|whitelist>
    """
    
    def __init__(self, filter_obj):
        super(DDBFilterMICommand, self).__init__("-ddb-filter-config")
        self.filter = filter_obj
    
    def invoke(self, arguments) -> Dict[str, object]:
        """Process MI command and return result"""
        try:
            if not arguments:
                return self._status_response()
            
            action = arguments[0]
            
            if action == "--enable":
                self.filter.config.enabled = True
                return self._success_response("Filter enabled")
            
            elif action == "--disable":
                self.filter.config.enabled = False
                return self._success_response("Filter disabled")
            
            elif action == "--add-function":
                if len(arguments) < 2:
                    return self._error_response("Missing pattern")
                pattern = arguments[1]
                match_type = self._parse_match_type_mi(arguments)
                self.filter.config.add_function_rule(pattern, match_type)
                return self._success_response(f"Added function rule: {pattern}")
            
            elif action == "--remove-function":
                if len(arguments) < 2:
                    return self._error_response("Missing pattern")
                pattern = arguments[1]
                self.filter.config.remove_function_rule(pattern)
                return self._success_response(f"Removed function rule: {pattern}")
            
            elif action == "--add-file":
                if len(arguments) < 2:
                    return self._error_response("Missing pattern")
                pattern = arguments[1]
                match_type = self._parse_match_type_mi(arguments)
                self.filter.config.add_file_rule(pattern, match_type)
                return self._success_response(f"Added file rule: {pattern}")
            
            elif action == "--remove-file":
                if len(arguments) < 2:
                    return self._error_response("Missing pattern")
                pattern = arguments[1]
                self.filter.config.remove_file_rule(pattern)
                return self._success_response(f"Removed file rule: {pattern}")
            
            elif action == "--preset-enable":
                if len(arguments) < 2:
                    return self._error_response("Missing preset name")
                preset_name = arguments[1]
                if self.filter.config.enable_preset(preset_name):
                    return self._success_response(f"Enabled preset: {preset_name}")
                else:
                    return self._error_response(f"Unknown preset: {preset_name}")
            
            elif action == "--preset-disable":
                if len(arguments) < 2:
                    return self._error_response("Missing preset name")
                preset_name = arguments[1]
                if self.filter.config.disable_preset(preset_name):
                    return self._success_response(f"Disabled preset: {preset_name}")
                else:
                    return self._error_response(f"Preset not active: {preset_name}")
            
            elif action == "--list-presets":
                return self._list_presets_response()
            
            elif action == "--status":
                return self._status_response()
            
            elif action == "--clear":
                self.filter.config.function_rules = []
                self.filter.config.file_rules = []
                self.filter.config.active_presets = set()
                return self._success_response("Cleared all rules")
            
            elif action == "--mode":
                if len(arguments) < 2:
                    return self._status_response()
                mode = arguments[1]
                if mode in ["blacklist", "whitelist"]:
                    self.filter.config.mode = mode
                    return self._success_response(f"Mode set to: {mode}")
                else:
                    return self._error_response("Invalid mode (must be blacklist or whitelist)")
            
            else:
                return self._error_response(f"Unknown action: {action}")
        
        except Exception as e:
            dbg(LogLevel.ERROR, f"Error in DDBFilterMICommand: {e}")
            return self._error_response(str(e))
    
    def _parse_match_type_mi(self, arguments: List[str]) -> MatchType:
        """Parse --match-type from MI arguments"""
        try:
            idx = arguments.index("--match-type")
            if idx + 1 < len(arguments):
                return MatchType(arguments[idx + 1])
        except (ValueError, KeyError):
            pass
        return MatchType.GLOB  # Default
    
    def _success_response(self, message: str) -> Dict[str, object]:
        """Create success response"""
        return {
            "message": "success",
            "info": message,
            "config": self.filter.config.to_dict()
        }
    
    def _error_response(self, error: str) -> Dict[str, object]:
        """Create error response"""
        return {
            "message": "error",
            "error": error,
            "config": self.filter.config.to_dict()
        }
    
    def _status_response(self) -> Dict[str, object]:
        """Create status response"""
        return {
            "message": "success",
            "config": self.filter.config.to_dict()
        }
    
    def _list_presets_response(self) -> Dict[str, object]:
        """Create preset list response"""
        presets = {}
        for name, preset in FRAMEWORK_PRESETS.items():
            presets[name] = {
                "description": preset["description"],
                "active": name in self.filter.config.active_presets,
                "functions": preset.get("functions", []),
                "files": preset.get("files", [])
            }
        return {
            "message": "success",
            "presets": presets,
            "config": self.filter.config.to_dict()
        }


# ============================================================================
# Frame Filter Registration
# ============================================================================

# Create and register the frame filter
ddb_frame_filter = DDBFrameFilter()
gdb.frame_filters[ddb_frame_filter.name] = ddb_frame_filter # type: ignore

# Create command interfaces
ddb_filter_mi_cmd = DDBFilterMICommand(ddb_frame_filter)
ddb_filter_cmd = DDBFilterCommand(ddb_frame_filter)

dbg(LogLevel.INFO, "DDB Frame Filter loaded and registered")