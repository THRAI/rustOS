#!/usr/bin/env python3
import sys
import os
import importlib.util

def load_test_module(test_name):
    """动态加载测试模块"""
    module_path = os.path.join(os.path.dirname(__file__), f"{test_name}_test.py")
    if not os.path.exists(module_path):
        return None
    spec = importlib.util.spec_from_file_location(f"{test_name}_test", module_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return getattr(module, f"{test_name}_test")()

def judge_output(test_name, output_lines):
    """评分单个测试输出"""
    test_obj = load_test_module(test_name)
    if not test_obj:
        return {"name": test_name, "error": "No test module found", "passed": 0, "all": 0}

    test_obj.start(output_lines)
    return test_obj.get_result()

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: run_judge.py <test_name> [output_file]")
        sys.exit(1)

    test_name = sys.argv[1]

    if len(sys.argv) > 2:
        with open(sys.argv[2], 'r') as f:
            lines = f.read().splitlines()
    else:
        lines = sys.stdin.read().splitlines()

    result = judge_output(test_name, lines)
    print(f"Test: {result['name']}")
    print(f"Score: {result['passed']}/{result['all']}")

    if result['passed'] == result['all']:
        print("PASS")
        sys.exit(0)
    else:
        print("FAIL")
        sys.exit(1)
