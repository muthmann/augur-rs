import ij.IJ;
import ij.Prefs;
import ij.gui.GenericDialog;
import ij.io.OpenDialog;
import ij.plugin.Macro_Runner;
import ij.plugin.PlugIn;

import java.awt.EventQueue;
import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;

public class AugurBridge implements PlugIn {
    private static final String PREF_PORT_KEY = "augur.bridge.port";
    private static final int DEFAULT_PORT = 57294;
    private static BridgeServer bridgeServer;

    @Override
    public void run(String arg) {
        synchronized (AugurBridge.class) {
            if (bridgeServer != null && bridgeServer.isRunning()) {
                IJ.showStatus("Augur Bridge already listening on 127.0.0.1:" + bridgeServer.getPort());
                return;
            }

            int port = promptForPort();
            if (port <= 0) {
                return;
            }

            try {
                bridgeServer = new BridgeServer(port);
                bridgeServer.start();
                Prefs.set(PREF_PORT_KEY, port);
                IJ.showStatus("Augur Bridge listening on 127.0.0.1:" + port);
                IJ.log("Augur Bridge listening on 127.0.0.1:" + port);
            } catch (IOException e) {
                IJ.error("Augur Bridge", "Failed to start bridge on port " + port + ": " + e.getMessage());
            }
        }
    }

    private static int promptForPort() {
        double savedPort = Prefs.get(PREF_PORT_KEY, DEFAULT_PORT);
        GenericDialog dialog = new GenericDialog("Augur Bridge");
        dialog.addMessage("Start the local TCP bridge that Augur uses to stream preview frames.");
        dialog.addNumericField("Port", savedPort, 0);
        dialog.showDialog();
        if (dialog.wasCanceled()) {
            return -1;
        }

        double portValue = dialog.getNextNumber();
        if (Double.isNaN(portValue) || portValue < 1 || portValue > 65535) {
            IJ.error("Augur Bridge", "Port must be between 1 and 65535.");
            return -1;
        }
        return (int) portValue;
    }

    private static void clearServer(BridgeServer server) {
        synchronized (AugurBridge.class) {
            if (bridgeServer == server) {
                bridgeServer = null;
            }
        }
    }

    private static final class BridgeServer implements Runnable {
        private final int port;
        private final ServerSocket serverSocket;
        private final Thread thread;

        BridgeServer(int port) throws IOException {
            this.port = port;
            this.serverSocket =
                new ServerSocket(port, 50, InetAddress.getByName("127.0.0.1"));
            this.thread = new Thread(this, "AugurBridge");
            this.thread.setDaemon(true);
        }

        int getPort() {
            return port;
        }

        boolean isRunning() {
            return !serverSocket.isClosed() && thread.isAlive();
        }

        void start() {
            thread.start();
        }

        @Override
        public void run() {
            try {
                while (!serverSocket.isClosed()) {
                    try (Socket socket = serverSocket.accept()) {
                        handleClient(socket);
                    } catch (IOException e) {
                        if (!serverSocket.isClosed()) {
                            IJ.log("Augur Bridge client error: " + e.getMessage());
                        }
                    }
                }
            } finally {
                try {
                    serverSocket.close();
                } catch (IOException ignored) {
                    // Nothing else to do while shutting down.
                }
                clearServer(this);
            }
        }

        private void handleClient(Socket socket) throws IOException {
            BufferedReader reader =
                new BufferedReader(
                    new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8)
                );
            String line;
            while ((line = reader.readLine()) != null) {
                handleCommand(line);
            }
        }

        private void handleCommand(String line) {
            String commandLine = line.trim();
            if (commandLine.isEmpty()) {
                return;
            }

            int split = commandLine.indexOf(' ');
            String command = split >= 0 ? commandLine.substring(0, split) : commandLine;
            String argument = split >= 0 ? commandLine.substring(split + 1) : "";

            try {
                runOnEventThread(command, argument);
            } catch (Exception e) {
                IJ.log("Augur Bridge command failed: " + e.getMessage());
            }
        }

        private void runOnEventThread(final String command, final String argument) throws Exception {
            Runnable action = new Runnable() {
                @Override
                public void run() {
                    executeCommand(command, argument);
                }
            };

            if (EventQueue.isDispatchThread()) {
                action.run();
                return;
            }

            EventQueue.invokeAndWait(action);
        }

        private void executeCommand(String command, String argument) {
            if ("eval".equals(command)) {
                new Macro_Runner().runMacro(argument, null);
                return;
            }
            if ("run".equals(command)) {
                IJ.run(argument);
                return;
            }
            if ("open".equals(command)) {
                IJ.open(argument);
                return;
            }
            if ("macro".equals(command)) {
                new Macro_Runner().runMacroFile(argument, null);
                return;
            }
            if ("user.dir".equals(command)) {
                System.setProperty("user.dir", argument);
                OpenDialog.setDefaultDirectory(argument);
                return;
            }
            IJ.log("Augur Bridge ignoring unsupported command: " + command);
        }
    }
}
