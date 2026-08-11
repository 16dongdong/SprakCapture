"""提供夹具运行命令，便于插件项目在 CI 中直接验证动作。"""

from __future__ import annotations

import argparse
import json

from .runner import loadPlugin
from .simulator import Simulator


def main() -> int:
    """解析 run-fixture 命令并输出动作 JSON；失败通过异常和非零退出码暴露。"""
    parser = argparse.ArgumentParser(description="Python 插件 SDK 工具")
    parser.add_argument("entry", help="module[:attribute]")
    parser.add_argument("fixture", help="RuntimeInvocation JSON 夹具")
    arguments = parser.parse_args()
    actions = Simulator(loadPlugin(arguments.entry)).runFixture(arguments.fixture)
    print(json.dumps([action.toDict() for action in actions], ensure_ascii=False, indent=2))
    return 0
