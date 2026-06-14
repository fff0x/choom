# SPDX-License-Identifier: GPL-3.0-or-later
{
  lib,
  buildPythonApplication,
  setuptools,
  asyncssh,
  pyte,
  aiohttp,
  pytestCheckHook,
  src ? ../.,
}:
buildPythonApplication {
  pname = "choom";
  version = "0.1.0";
  pyproject = true;

  inherit src;

  build-system = [ setuptools ];

  dependencies = [
    asyncssh
    pyte
    aiohttp
  ];

  # The integration/web/reconnect tests spawn PTYs, subprocesses and bind sockets,
  # which the Nix build sandbox forbids. Run only the offline unit tests here.
  nativeCheckInputs = [ pytestCheckHook ];
  enabledTestPaths = [ "tests/test_unit.py" ];

  pythonImportsCheck = [
    "choom"
    "choom.cli"
    "choom.server"
    "choom.web"
  ];

  meta = {
    description = "tmate without tmux: minimal SSH terminal sharing relay";
    homepage = "https://this-is-fine.io";
    license = lib.licenses.gpl3Plus;
    mainProgram = "choom";
    platforms = lib.platforms.unix;
  };
}
