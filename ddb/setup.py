from setuptools import setup, find_packages

with open('requirements.txt') as f:
    required = f.read().splitlines()

setup(
    name="ddb",
    version="0.1",
    packages=find_packages(),
    install_requires=required,
    author="NSL",
    author_email="yiboyan@usc.edu",
    description="Interactive Distributed Debugger (DDB)",
    long_description=open("README.md").read(),
    long_description_content_type="text/markdown",
    url="",
    package_data={
        "ddb": [
            "conf/*.conf",
            "gdb_ext/*.py"
        ],
    },
    entry_points={
        'console_scripts': [
            'ddb = ddb.main:main',
        ],
    },
)
