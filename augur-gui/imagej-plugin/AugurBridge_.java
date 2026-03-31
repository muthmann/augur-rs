import ij.IJ;
import ij.ImagePlus;
import ij.ImageStack;
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
import java.util.ArrayList;
import java.util.Locale;
import java.util.concurrent.ConcurrentLinkedQueue;
import java.util.concurrent.atomic.AtomicBoolean;

public class AugurBridge_ implements PlugIn {
    private static final String PREF_PORT_KEY = "augur.bridge.port";
    private static final String PREF_MAX_FRAMES_KEY = "augur.bridge.max_frames";
    private static final String PREF_MODE_KEY = "augur.bridge.mode";
    private static final int DEFAULT_PORT = 57294;
    private static final int DEFAULT_MAX_FRAMES = 500;
    private static final String MODE_TIMELINE = "Timeline (stack)";
    private static final String MODE_LIVE_ONLY = "Live only (single frame)";
    private static final String LIVE_TITLE = "augur_live";
    private static final String ARCHIVE_TITLE_PREFIX = "augur_live_archive";
    private static BridgeServer bridgeServer;

    @Override
    public void run(String arg) {
        synchronized (AugurBridge_.class) {
            if (bridgeServer != null && bridgeServer.isRunning()) {
                IJ.showStatus("Augur Bridge already listening on 127.0.0.1:" + bridgeServer.getPort());
                return;
            }

            BridgeSettings settings = promptForSettings();
            if (settings == null) {
                return;
            }

            try {
                bridgeServer = new BridgeServer(settings);
                bridgeServer.start();
                Prefs.set(PREF_PORT_KEY, settings.port);
                Prefs.set(PREF_MAX_FRAMES_KEY, settings.maxFrames);
                Prefs.set(PREF_MODE_KEY, settings.timelineMode ? MODE_TIMELINE : MODE_LIVE_ONLY);
                IJ.showStatus("Augur Bridge listening on 127.0.0.1:" + settings.port);
                IJ.log("Augur Bridge listening on 127.0.0.1:" + settings.port);
            } catch (IOException e) {
                IJ.error("Augur Bridge", "Failed to start bridge on port " + settings.port + ": " + e.getMessage());
            }
        }
    }

    private static BridgeSettings promptForSettings() {
        double savedPort = Prefs.get(PREF_PORT_KEY, DEFAULT_PORT);
        double savedMaxFrames = Prefs.get(PREF_MAX_FRAMES_KEY, DEFAULT_MAX_FRAMES);
        String savedMode = Prefs.get(PREF_MODE_KEY, MODE_TIMELINE);

        GenericDialog dialog = new GenericDialog("Augur Bridge");
        dialog.addMessage("Start the local TCP bridge that Augur uses to stream preview frames.");
        dialog.addNumericField("Port", savedPort, 0);
        dialog.addChoice("Mode", new String[] { MODE_TIMELINE, MODE_LIVE_ONLY }, savedMode);
        dialog.addNumericField("Max frames", savedMaxFrames, 0);
        dialog.showDialog();
        if (dialog.wasCanceled()) {
            return null;
        }

        double portValue = dialog.getNextNumber();
        String modeValue = dialog.getNextChoice();
        double maxFramesValue = dialog.getNextNumber();
        if (Double.isNaN(portValue) || portValue < 1 || portValue > 65535) {
            IJ.error("Augur Bridge", "Port must be between 1 and 65535.");
            return null;
        }
        if (Double.isNaN(maxFramesValue) || maxFramesValue < 1 || maxFramesValue > Integer.MAX_VALUE) {
            IJ.error("Augur Bridge", "Max frames must be at least 1.");
            return null;
        }

        return new BridgeSettings(
            (int) portValue,
            (int) maxFramesValue,
            MODE_TIMELINE.equals(modeValue)
        );
    }

    private static void clearServer(BridgeServer server) {
        synchronized (AugurBridge_.class) {
            if (bridgeServer == server) {
                bridgeServer = null;
            }
        }
    }

    private static final class BridgeSettings {
        final int port;
        final int maxFrames;
        final boolean timelineMode;

        BridgeSettings(int port, int maxFrames, boolean timelineMode) {
            this.port = port;
            this.maxFrames = maxFrames;
            this.timelineMode = timelineMode;
        }
    }

    private static final class FrameData {
        final int width;
        final int height;
        final double nmPerPixel;
        final long seq;
        final long timestampUs;
        final short[] pixels;

        FrameData(int width, int height, double nmPerPixel, long seq, long timestampUs, short[] pixels) {
            this.width = width;
            this.height = height;
            this.nmPerPixel = nmPerPixel;
            this.seq = seq;
            this.timestampUs = timestampUs;
            this.pixels = pixels;
        }
    }

    private static final class BridgeServer implements Runnable {
        private final int port;
        private final boolean timelineMode;
        private final ServerSocket serverSocket;
        private final Thread thread;
        private final ConcurrentLinkedQueue<FrameData> pendingFrames =
            new ConcurrentLinkedQueue<FrameData>();
        private final AtomicBoolean frameDrainScheduled = new AtomicBoolean(false);
        private final FrameAccumulator frameAccumulator;
        private ImagePlus liveImage;
        private long legacyFrameCounter = 1L;

        BridgeServer(BridgeSettings settings) throws IOException {
            this.port = settings.port;
            this.timelineMode = settings.timelineMode;
            this.serverSocket =
                new ServerSocket(settings.port, 50, InetAddress.getByName("127.0.0.1"));
            this.thread = new Thread(this, "AugurBridge");
            this.thread.setDaemon(true);
            this.frameAccumulator = settings.timelineMode
                ? new FrameAccumulator(settings.maxFrames)
                : null;
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
            String[] parts = header.trim().split("\\s+");
            if (parts.length < 3) {
                IJ.log("Augur Bridge: malformed frame header: " + header);
                return;
            }

            final int width;
            final int height;
            final double nmPerPixel;
            final long seq;
            final long timestampUs;
            try {
                width = Integer.parseInt(parts[0]);
                height = Integer.parseInt(parts[1]);
                nmPerPixel = Double.parseDouble(parts[2]);
                seq = parts.length >= 4 ? Long.parseLong(parts[3]) : legacyFrameCounter++;
                timestampUs = parts.length >= 5 ? Long.parseLong(parts[4]) : 0L;
            } catch (NumberFormatException e) {
                IJ.log("Augur Bridge: bad frame header numbers: " + header);
                return;
            }

            if (width <= 0 || height <= 0) {
                IJ.log("Augur Bridge: frame dimensions must be positive: " + header);
                return;
            }

            long numPixelsLong = (long) width * (long) height;
            if (numPixelsLong <= 0L || numPixelsLong > (Integer.MAX_VALUE / 2)) {
                IJ.log("Augur Bridge: frame is too large: " + header);
                return;
            }

            int numPixels = (int) numPixelsLong;
            byte[] rawBytes = new byte[numPixels * 2];
            try {
                dataIn.readFully(rawBytes);
            } catch (IOException e) {
                IJ.log("Augur Bridge: failed to read frame pixels: " + e.getMessage());
                return;
            }

            final short[] pixels = new short[numPixels];
            ByteBuffer.wrap(rawBytes).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer().get(pixels);
            enqueueFrame(new FrameData(width, height, nmPerPixel, seq, timestampUs, pixels));
        }

        private void enqueueFrame(FrameData frame) {
            pendingFrames.add(frame);
            scheduleFrameDrain();
        }

        private void scheduleFrameDrain() {
            if (frameDrainScheduled.compareAndSet(false, true)) {
                EventQueue.invokeLater(new Runnable() {
                    @Override
                    public void run() {
                        drainPendingFramesOnEDT();
                    }
                });
            }
        }

        private void drainPendingFramesOnEDT() {
            try {
                if (timelineMode) {
                    ArrayList<FrameData> batch = new ArrayList<FrameData>();
                    FrameData frame;
                    while ((frame = pendingFrames.poll()) != null) {
                        batch.add(frame);
                    }
                    if (!batch.isEmpty()) {
                        frameAccumulator.addFrames(batch);
                    }
                } else {
                    FrameData latest = null;
                    FrameData frame;
                    while ((frame = pendingFrames.poll()) != null) {
                        latest = frame;
                    }
                    if (latest != null) {
                        updateLiveImage(latest.width, latest.height, latest.nmPerPixel, latest.pixels);
                    }
                }
            } catch (Exception e) {
                IJ.log("Augur Bridge: display update failed: " + e.getMessage());
            } finally {
                frameDrainScheduled.set(false);
                if (!pendingFrames.isEmpty()) {
                    scheduleFrameDrain();
                }
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
                applyCalibration(liveImage, nmPerPixel);
                liveImage.show();
            } else {
                liveImage.getProcessor().setPixels(pixels);
                applyCalibration(liveImage, nmPerPixel);
                liveImage.updateAndDraw();
            }
        }

        private void applyCalibration(ImagePlus image, double nmPerPixel) {
            Calibration cal = image.getCalibration();
            cal.pixelWidth = nmPerPixel;
            cal.pixelHeight = nmPerPixel;
            cal.setUnit("nm");
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

        private final class FrameAccumulator {
            private final int maxFrames;
            private ImageStack stack;
            private ImagePlus liveImage;
            private int currentWidth = -1;
            private int currentHeight = -1;
            private int archivedStackCount = 0;
            private double currentNmPerPixel = Double.NaN;

            FrameAccumulator(int maxFrames) {
                this.maxFrames = maxFrames;
            }

            void addFrames(ArrayList<FrameData> frames) {
                boolean followLatest = shouldFollowLatest();
                int retainedSlice = currentDisplayedSlice();
                int droppedFromFront = 0;

                for (int i = 0; i < frames.size(); i++) {
                    FrameData frame = frames.get(i);
                    if (hasStack() && dimensionsChanged(frame.width, frame.height)) {
                        archiveCurrentStack();
                        followLatest = true;
                        retainedSlice = 1;
                        droppedFromFront = 0;
                    }

                    ensureStack(frame.width, frame.height);
                    currentNmPerPixel = frame.nmPerPixel;
                    stack.addSlice(sliceLabelFor(frame),
                        new ShortProcessor(frame.width, frame.height, frame.pixels, null));
                    while (stack.getSize() > maxFrames) {
                        stack.deleteSlice(1);
                        droppedFromFront++;
                    }
                }

                if (hasStack()) {
                    updateDisplay(followLatest, retainedSlice, droppedFromFront);
                }
            }

            private boolean hasStack() {
                return stack != null && stack.getSize() > 0;
            }

            private boolean dimensionsChanged(int width, int height) {
                return width != currentWidth || height != currentHeight;
            }

            private void ensureStack(int width, int height) {
                if (stack == null) {
                    stack = new ImageStack(width, height);
                    currentWidth = width;
                    currentHeight = height;
                }
            }

            private boolean shouldFollowLatest() {
                if (!hasStack()) {
                    return true;
                }
                if (liveImage == null || liveImage.getWindow() == null || liveImage.getWindow().isClosed()) {
                    return true;
                }
                return liveImage.getCurrentSlice() >= stack.getSize();
            }

            private int currentDisplayedSlice() {
                if (liveImage == null || liveImage.getWindow() == null || liveImage.getWindow().isClosed()) {
                    return 1;
                }
                return liveImage.getCurrentSlice();
            }

            private void updateDisplay(boolean followLatest, int retainedSlice, int droppedFromFront) {
                if (stack == null) {
                    return;
                }

                boolean needNewImage = liveImage == null
                    || liveImage.getWindow() == null
                    || liveImage.getWindow().isClosed();
                if (needNewImage) {
                    liveImage = new ImagePlus(LIVE_TITLE, stack);
                    applyCalibration(liveImage, currentNmPerPixel);
                    liveImage.show();
                } else {
                    liveImage.setStack(LIVE_TITLE, stack);
                    applyCalibration(liveImage, currentNmPerPixel);
                }

                int lastSlice = stack.getSize();
                if (followLatest) {
                    liveImage.setSlice(lastSlice);
                } else {
                    int adjustedSlice = retainedSlice - droppedFromFront;
                    adjustedSlice = Math.max(1, Math.min(lastSlice, adjustedSlice));
                    liveImage.setSlice(adjustedSlice);
                }
                liveImage.updateAndDraw();
            }

            private void archiveCurrentStack() {
                if (!hasStack()) {
                    reset();
                    return;
                }

                String archivedTitle = ARCHIVE_TITLE_PREFIX + "_" + (++archivedStackCount);
                if (liveImage != null && liveImage.getWindow() != null && !liveImage.getWindow().isClosed()) {
                    liveImage.setTitle(archivedTitle);
                    liveImage.updateAndDraw();
                } else {
                    ImagePlus archivedImage = new ImagePlus(archivedTitle, stack);
                    applyCalibration(archivedImage, currentNmPerPixel);
                    archivedImage.show();
                }
                reset();
            }

            private void reset() {
                stack = null;
                liveImage = null;
                currentWidth = -1;
                currentHeight = -1;
                currentNmPerPixel = Double.NaN;
            }

            private String sliceLabelFor(FrameData frame) {
                double seconds = frame.timestampUs / 1_000_000.0;
                return String.format(Locale.US, "t=%.3fs (#%d)", seconds, frame.seq);
            }
        }
    }
}
