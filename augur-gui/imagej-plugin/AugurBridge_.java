import ij.IJ;
import ij.ImagePlus;
import ij.Prefs;
import ij.gui.GenericDialog;
import ij.io.OpenDialog;
import ij.measure.Calibration;
import ij.plugin.Macro_Runner;
import ij.plugin.PlugIn;
import ij.process.ShortProcessor;

import java.awt.EventQueue;
import java.io.DataInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.lang.reflect.InvocationTargetException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;

public class AugurBridge_ implements PlugIn {
    private static final String PREF_PORT_KEY = "augur.bridge.port";
    private static final int DEFAULT_PORT = 57294;
    private static final String LIVE_TITLE = "augur_live";
    private static BridgeServer bridgeServer;

    @Override
    public void run(String arg) {
        synchronized (AugurBridge_.class) {
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
        synchronized (AugurBridge_.class) {
            if (bridgeServer == server) {
                bridgeServer = null;
            }
        }
    }

    private static final class BridgeServer implements Runnable {
        private final int port;
        private final ServerSocket serverSocket;
        private final Thread thread;
        private ImagePlus liveImage;

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
                }
                clearServer(this);
            }
        }

        private void handleClient(Socket socket) throws IOException {
            InputStream rawIn = socket.getInputStream();
            DataInputStream dataIn = new DataInputStream(rawIn);

            // Read lines byte-by-byte so we can switch to binary reads for frame data.
            StringBuilder sb = new StringBuilder();
            while (true) {
                int b = dataIn.read();
                if (b == -1) {
                    break;
                }
                if (b == '\n') {
                    String line = sb.toString().trim();
                    sb.setLength(0);
                    if (!line.isEmpty()) {
                        handleLine(line, dataIn);
                    }
                } else {
                    sb.append((char) b);
                }
            }
        }

        private void handleLine(String line, DataInputStream dataIn) {
            int split = line.indexOf(' ');
            String command = split >= 0 ? line.substring(0, split) : line;
            String argument = split >= 0 ? line.substring(split + 1) : "";

            if ("frame".equals(command)) {
                handleFrameCommand(argument, dataIn);
                return;
            }

            try {
                final String cmd = command;
                final String arg = argument;
                invokeOnEDT(new Runnable() {
                    @Override
                    public void run() {
                        executeTextCommand(cmd, arg);
                    }
                });
            } catch (Exception e) {
                IJ.log("Augur Bridge command failed: " + e.getMessage());
            }
        }

        private void handleFrameCommand(String header, DataInputStream dataIn) {
            // Header format: "<width> <height> <nm_per_pixel>"
            String[] parts = header.split(" ");
            if (parts.length < 3) {
                IJ.log("Augur Bridge: malformed frame header: " + header);
                return;
            }

            final int width;
            final int height;
            final double nmPerPixel;
            try {
                width = Integer.parseInt(parts[0]);
                height = Integer.parseInt(parts[1]);
                nmPerPixel = Double.parseDouble(parts[2]);
            } catch (NumberFormatException e) {
                IJ.log("Augur Bridge: bad frame header numbers: " + header);
                return;
            }

            int numPixels = width * height;
            byte[] rawBytes = new byte[numPixels * 2];
            try {
                dataIn.readFully(rawBytes);
            } catch (IOException e) {
                IJ.log("Augur Bridge: failed to read frame pixels: " + e.getMessage());
                return;
            }

            // Convert little-endian bytes to short array.
            final short[] pixels = new short[numPixels];
            ByteBuffer.wrap(rawBytes).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer().get(pixels);

            try {
                invokeOnEDT(new Runnable() {
                    @Override
                    public void run() {
                        updateLiveImage(width, height, nmPerPixel, pixels);
                    }
                });
            } catch (Exception e) {
                IJ.log("Augur Bridge: display update failed: " + e.getMessage());
            }
        }

        private void updateLiveImage(int width, int height, double nmPerPixel, short[] pixels) {
            boolean needNewImage = liveImage == null
                || liveImage.getWindow() == null
                || liveImage.getWindow().isClosed()
                || liveImage.getWidth() != width
                || liveImage.getHeight() != height;

            if (needNewImage) {
                if (liveImage != null && liveImage.getWindow() != null
                        && !liveImage.getWindow().isClosed()) {
                    liveImage.close();
                }
                ShortProcessor sp = new ShortProcessor(width, height, pixels, null);
                liveImage = new ImagePlus(LIVE_TITLE, sp);
                Calibration cal = liveImage.getCalibration();
                cal.pixelWidth = nmPerPixel;
                cal.pixelHeight = nmPerPixel;
                cal.setUnit("nm");
                liveImage.show();
            } else {
                liveImage.getProcessor().setPixels(pixels);
                Calibration cal = liveImage.getCalibration();
                if (cal.pixelWidth != nmPerPixel) {
                    cal.pixelWidth = nmPerPixel;
                    cal.pixelHeight = nmPerPixel;
                    cal.setUnit("nm");
                }
                liveImage.updateAndDraw();
            }
        }

        private void invokeOnEDT(Runnable action)
                throws InterruptedException, InvocationTargetException {
            if (EventQueue.isDispatchThread()) {
                action.run();
            } else {
                EventQueue.invokeAndWait(action);
            }
        }

        private void executeTextCommand(String command, String argument) {
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
