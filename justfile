# choom task runner — `just <task>` (https://github.com/casey/just)
# Inside the flake devShell (direnv) python/pytest/nix are on PATH.

venv := ".venv"
py   := venv / "bin/python"

# list tasks
default:
    @just --list

# create venv + install (editable, with test deps)
setup:
    python3 -m venv {{venv}}
    {{py}} -m pip install -e '.[test]'

# run the test suite
test *args:
    {{py}} -m pytest {{args}}

# build the choom package with nix
build:
    nix build .#choom

# run choom from the flake (e.g. `just run host`)
run *args:
    nix run .#choom -- {{args}}

# build the relay container image
image tag="choom/relay:dev":
    docker build -t {{tag}} .

# build a portable single-file binary into ./downloads (Nuitka, default)
binary:
    ./tools/build-binaries.sh

# format nix files
fmt:
    nix fmt

# remove build artifacts
clean:
    rm -rf {{venv}} downloads result .pytest_cache **/__pycache__
