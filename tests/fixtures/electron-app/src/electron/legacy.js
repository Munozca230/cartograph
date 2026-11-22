const { ipcMain } = require("electron");

function onPing() {
  return "pong";
}

module.exports = {
  ping() {
    return onPing();
  },
  later: () => setTimeout(onPing, 1),
};

ipcMain.on("legacy:ping", onPing);
