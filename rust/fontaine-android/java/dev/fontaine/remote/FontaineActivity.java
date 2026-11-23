package dev.fontaine.remote;

import android.app.NativeActivity;
import android.content.Context;
import android.os.Build;
import android.os.Bundle;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.text.SpannableStringBuilder;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.view.inputmethod.TextAttribute;

/**
 * NativeActivity's content view has no input connection, so the keyboard falls back to sending key
 * events and cannot swipe, suggest or move the cursor. This subclass hosts an invisible editor view
 * whose connection edits a mirror of the Rust field and reports every change back over JNI.
 */
public class FontaineActivity extends NativeActivity {
    static {
        // NativeActivity opens the library only to find ANativeActivity_onCreate; the JVM binds
        // the native methods below only for libraries loaded through System.loadLibrary.
        System.loadLibrary("fontaine_android");
    }

    /** Text, selection and composing span as the editor holds them; -1 means no composing span. */
    static native void nativeImeState(
            String text, int selStart, int selEnd, int compStart, int compEnd, int generation);

    static native void nativeImeSubmit();

    private ImeView ime;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        ime = new ImeView(this);
        addContentView(ime, new ViewGroup.LayoutParams(1, 1));
        // A view that is only focusable cannot take focus in touch mode, and the input method
        // manager serves only the focused view.
        ime.setFocusableInTouchMode(true);
        ime.requestFocus();
    }

    /** Replaces the editor's content with the field's; called from the native thread. */
    public void setImeState(
            final String text,
            final int selStart,
            final int selEnd,
            final int compStart,
            final int compEnd,
            final int generation,
            final boolean wholesale) {
        runOnUiThread(
                new Runnable() {
                    @Override
                    public void run() {
                        if (ime != null) {
                            ime.apply(text, selStart, selEnd, compStart, compEnd, generation, wholesale);
                        }
                    }
                });
    }

    public void showKeyboard() {
        runOnUiThread(
                new Runnable() {
                    @Override
                    public void run() {
                        if (ime != null) {
                            ime.show();
                        }
                    }
                });
    }

    public void hideKeyboard() {
        runOnUiThread(
                new Runnable() {
                    @Override
                    public void run() {
                        if (ime != null) {
                            ime.hide();
                        }
                    }
                });
    }

    private static final class ImeView extends View {
        private final SpannableStringBuilder mirror = new SpannableStringBuilder();
        // The generation of the last Rust push applied, echoed by every report so Rust can tell a
        // report built before that push from one built after it.
        private int generation;
        // A Rust push that arrived mid-batch waits for the batch's report, so it cannot wipe the
        // batch's edits and stamp its generation on them; only the newest is kept.
        private Connection live;
        private Runnable deferred;
        // Set while a deferred push is replayed from onCreateInputConnection, whose EditorInfo
        // already carries the state, so the input method must not be told again.
        private boolean quiet;

        ImeView(Context context) {
            super(context);
            setFocusable(true);
            setBackgroundColor(0);
            Selection.setSelection(mirror, 0, 0);
        }

        private InputMethodManager manager() {
            return (InputMethodManager) getContext().getSystemService(Context.INPUT_METHOD_SERVICE);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
            // One logical line that wraps: Enter submits, so a multi-line editor would make
            // keyboards insert newlines instead of offering Send.
            runDeferred(true);
            outAttrs.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_CAP_SENTENCES;
            outAttrs.imeOptions =
                    EditorInfo.IME_ACTION_SEND
                            | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                            | EditorInfo.IME_FLAG_NO_FULLSCREEN;
            outAttrs.initialSelStart = Selection.getSelectionStart(mirror);
            outAttrs.initialSelEnd = Selection.getSelectionEnd(mirror);
            Connection connection = new Connection(this);
            live = connection;
            outAttrs.initialCapsMode = connection.getCursorCapsMode(outAttrs.inputType);
            if (Build.VERSION.SDK_INT >= 30) {
                outAttrs.setInitialSurroundingText(mirror);
            }
            return connection;
        }

        private void runDeferred(boolean silently) {
            live = null;
            Runnable push = deferred;
            deferred = null;
            if (push != null) {
                quiet = silently;
                try {
                    push.run();
                } finally {
                    quiet = false;
                }
            }
        }

        void apply(
                final String text,
                final int selStart,
                final int selEnd,
                final int compStart,
                final int compEnd,
                final int generation,
                final boolean wholesale) {
            if (live != null && live.batchDepth > 0) {
                deferred =
                        new Runnable() {
                            @Override
                            public void run() {
                                applyNow(
                                        text, selStart, selEnd, compStart, compEnd, generation, wholesale);
                            }
                        };
                return;
            }
            applyNow(text, selStart, selEnd, compStart, compEnd, generation, wholesale);
        }

        private void applyNow(
                String text,
                int selStart,
                int selEnd,
                int compStart,
                int compEnd,
                int generation,
                boolean wholesale) {
            this.generation = generation;
            mirror.replace(0, mirror.length(), text);
            int length = mirror.length();
            Selection.setSelection(mirror, clamp(selStart, length), clamp(selEnd, length));
            BaseInputConnection.removeComposingSpans(mirror);
            if (compStart >= 0 && compEnd > compStart && compEnd <= length) {
                BaseInputConnection.setComposingSpan(mirror, compStart, compEnd);
            }
            InputMethodManager manager = manager();
            if (manager == null || quiet) {
                return;
            }
            if (wholesale) {
                manager.restartInput(this);
            } else {
                manager.updateSelection(
                        this,
                        Selection.getSelectionStart(mirror),
                        Selection.getSelectionEnd(mirror),
                        BaseInputConnection.getComposingSpanStart(mirror),
                        BaseInputConnection.getComposingSpanEnd(mirror));
            }
        }

        private static int clamp(int value, int length) {
            return Math.max(0, Math.min(value, length));
        }

        void show() {
            requestFocus();
            InputMethodManager manager = manager();
            if (manager != null) {
                manager.showSoftInput(this, 0);
            }
        }

        void hide() {
            InputMethodManager manager = manager();
            if (manager != null) {
                manager.hideSoftInputFromWindow(getWindowToken(), 0);
            }
        }

        /** Tells the input method and Rust what the mirror holds now. */
        void report() {
            int selStart = Selection.getSelectionStart(mirror);
            int selEnd = Selection.getSelectionEnd(mirror);
            int compStart = BaseInputConnection.getComposingSpanStart(mirror);
            int compEnd = BaseInputConnection.getComposingSpanEnd(mirror);
            // BaseInputConnection does not do this, and without it the keyboard's idea of the
            // cursor and composing word goes stale.
            InputMethodManager manager = manager();
            if (manager != null) {
                manager.updateSelection(this, selStart, selEnd, compStart, compEnd);
            }
            nativeImeState(mirror.toString(), selStart, selEnd, compStart, compEnd, generation);
        }

        private final class Connection extends BaseInputConnection {
            // BaseInputConnection keeps no batch depth; while it is above 0 the mirror is only
            // marked dirty and reported once when the keyboard ends the batch.
            private int batchDepth;
            private boolean dirty;

            Connection(View view) {
                super(view, true);
            }

            @Override
            public Editable getEditable() {
                return mirror;
            }

            private void changed() {
                dirty = true;
                if (batchDepth == 0) {
                    flush();
                }
            }

            private void flush() {
                dirty = false;
                report();
            }

            @Override
            public boolean beginBatchEdit() {
                batchDepth++;
                return true;
            }

            @Override
            public boolean endBatchEdit() {
                if (batchDepth > 0) {
                    batchDepth--;
                }
                if (batchDepth == 0 && dirty) {
                    flush();
                }
                if (batchDepth == 0 && live == this) {
                    Runnable push = deferred;
                    deferred = null;
                    if (push != null) {
                        push.run();
                    }
                }
                return batchDepth > 0;
            }

            @Override
            public boolean commitText(CharSequence text, int newCursorPosition) {
                boolean done = super.commitText(text, newCursorPosition);
                changed();
                return done;
            }

            @Override
            public boolean setComposingText(CharSequence text, int newCursorPosition) {
                boolean done = super.setComposingText(text, newCursorPosition);
                changed();
                return done;
            }

            @Override
            public boolean setComposingRegion(int start, int end) {
                boolean done = super.setComposingRegion(start, end);
                changed();
                return done;
            }

            @Override
            public boolean finishComposingText() {
                boolean done = super.finishComposingText();
                changed();
                return done;
            }

            @Override
            public boolean deleteSurroundingText(int beforeLength, int afterLength) {
                boolean done = super.deleteSurroundingText(beforeLength, afterLength);
                changed();
                return done;
            }

            @Override
            public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
                boolean done = super.deleteSurroundingTextInCodePoints(beforeLength, afterLength);
                changed();
                return done;
            }

            @Override
            public boolean setSelection(int start, int end) {
                boolean done = super.setSelection(start, end);
                changed();
                return done;
            }

            @Override
            public boolean replaceText(
                    int start,
                    int end,
                    CharSequence text,
                    int newCursorPosition,
                    TextAttribute textAttribute) {
                boolean done = super.replaceText(start, end, text, newCursorPosition, textAttribute);
                changed();
                return done;
            }

            @Override
            public boolean performEditorAction(int actionCode) {
                if (actionCode == EditorInfo.IME_ACTION_SEND
                        || actionCode == EditorInfo.IME_ACTION_DONE
                        || actionCode == EditorInfo.IME_ACTION_GO) {
                    nativeImeSubmit();
                    return true;
                }
                return super.performEditorAction(actionCode);
            }
        }
    }
}
