package com.ripoteam.launcher;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.text.InputType;
import android.view.Gravity;
import android.view.View;
import android.view.ViewOutlineProvider;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

public final class MainActivity extends Activity {
    private final ExecutorService network = Executors.newSingleThreadExecutor();
    private int blue;
    private int orange;
    private int text;
    private int muted;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        blue = getColor(R.color.launcher_blue);
        orange = getColor(R.color.launcher_orange);
        text = getColor(R.color.launcher_text);
        muted = getColor(R.color.launcher_muted);
        showAuthentication();
    }

    @Override
    protected void onDestroy() {
        network.shutdownNow();
        super.onDestroy();
    }

    private void showAuthentication() {
        LinearLayout page = page();
        page.addView(brand(false));
        TextView subtitle = label("Sign in, create an account, or continue as a guest.", 16, muted);
        subtitle.setPadding(0, dp(10), 0, dp(22));
        page.addView(subtitle);

        LinearLayout card = card();
        EditText email = field("Email", false);
        EditText password = field("Password", true);
        TextView error = label("", 13, Color.rgb(190, 36, 36));
        Button signIn = primaryButton("Sign in");
        Button signUp = secondaryButton("Create account");
        Button guest = secondaryButton("Continue as guest");
        card.addView(email);
        card.addView(password);
        card.addView(error);
        card.addView(signIn);
        card.addView(signUp);
        card.addView(guest);
        page.addView(card);

        TextView note = label("Guests can play, but account sync, private rooms, and cloud photos require a Flux Rec account.", 13, muted);
        note.setPadding(0, dp(18), 0, 0);
        page.addView(note);

        signIn.setOnClickListener(v -> authenticate(email, password, error, signIn, false));
        signUp.setOnClickListener(v -> authenticate(email, password, error, signUp, true));
        guest.setOnClickListener(v -> showLibrary("Guest", true));
        setContentView(scroll(page));
    }

    private void authenticate(EditText email, EditText password, TextView error, Button action, boolean create) {
        String address = email.getText().toString().trim();
        String secret = password.getText().toString();
        if (!address.contains("@") || address.startsWith("@") || secret.length() < 6) {
            error.setText("Enter an email and a password of at least 6 characters.");
            return;
        }
        if (BuildConfig.FIREBASE_WEB_API_KEY.isEmpty()) {
            error.setText("Account sign-in is not configured in this build.");
            return;
        }
        action.setEnabled(false);
        action.setText(create ? "Creating…" : "Signing in…");
        error.setText("");
        network.execute(() -> {
            try {
                String operation = create ? "signUp" : "signInWithPassword";
                URL url = new URL("https://identitytoolkit.googleapis.com/v1/accounts:" + operation + "?key=" + BuildConfig.FIREBASE_WEB_API_KEY);
                HttpURLConnection connection = (HttpURLConnection) url.openConnection();
                connection.setRequestMethod("POST");
                connection.setRequestProperty("Content-Type", "application/json");
                connection.setConnectTimeout(15_000);
                connection.setReadTimeout(15_000);
                connection.setDoOutput(true);
                JSONObject request = new JSONObject()
                    .put("email", address)
                    .put("password", secret)
                    .put("returnSecureToken", true);
                try (OutputStream output = connection.getOutputStream()) {
                    output.write(request.toString().getBytes(StandardCharsets.UTF_8));
                }
                int status = connection.getResponseCode();
                JSONObject response = new JSONObject(read(status >= 200 && status < 300 ? connection.getInputStream() : connection.getErrorStream()));
                if (status < 200 || status >= 300) {
                    JSONObject remoteError = response.optJSONObject("error");
                    throw new IllegalStateException(remoteError == null ? "Account request failed" : remoteError.optString("message", "Account request failed"));
                }
                String name = response.optString("displayName", address.substring(0, address.indexOf('@')));
                runOnUiThread(() -> showLibrary(name, false));
            } catch (Exception ex) {
                runOnUiThread(() -> {
                    error.setText(ex.getMessage() == null ? "Account request failed." : ex.getMessage().replace('_', ' '));
                    action.setEnabled(true);
                    action.setText(create ? "Create account" : "Sign in");
                });
            }
        });
    }

    private void showLibrary(String username, boolean guest) {
        LinearLayout page = page();
        LinearLayout top = row();
        top.addView(brand(true), new LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1));
        Button account = secondaryButton(guest ? "Guest" : username);
        account.setOnClickListener(v -> showAuthentication());
        top.addView(account);
        page.addView(top);

        TextView title = label("Your library", 32, text);
        title.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        title.setPadding(0, dp(28), 0, dp(18));
        page.addView(title);

        LinearLayout hero = card();
        TextView gameTitle = label("FLUX REC", 36, Color.WHITE);
        gameTitle.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        TextView tagline = label("Explore, play, and create together.", 17, Color.WHITE);
        TextView status = label("Ready", 14, muted);
        Button play = primaryButton("Play");
        hero.setBackground(roundedGradient(orange, blue, 28));
        hero.addView(gameTitle);
        hero.addView(tagline);
        LinearLayout actions = card();
        actions.addView(status);
        actions.addView(play);
        page.addView(hero);
        page.addView(actions);

        TextView runtimeNote = label("This APK is the mobile launcher shell. A native Android Flux Rec runtime must be compiled into the game module before Play can start it inside this app.", 13, muted);
        runtimeNote.setPadding(0, dp(18), 0, 0);
        page.addView(runtimeNote);

        play.setOnClickListener(v -> {
            Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse("fluxrec://play"));
            if (intent.resolveActivity(getPackageManager()) != null) {
                startActivity(intent);
                status.setText("Launching Flux Rec…");
            } else {
                status.setText("Flux Rec mobile runtime is not installed in this build.");
            }
        });
        setContentView(scroll(page));
    }

    private ScrollView scroll(View child) {
        ScrollView scroll = new ScrollView(this);
        scroll.setFillViewport(true);
        scroll.addView(child);
        return scroll;
    }

    private LinearLayout page() {
        LinearLayout page = new LinearLayout(this);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setPadding(dp(22), dp(38), dp(22), dp(38));
        page.setBackgroundColor(getColor(R.color.launcher_background));
        return page;
    }

    private LinearLayout card() {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setPadding(dp(20), dp(20), dp(20), dp(20));
        card.setBackground(rounded(Color.WHITE, 24));
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        params.setMargins(0, dp(10), 0, dp(10));
        card.setLayoutParams(params);
        card.setElevation(dp(6));
        return card;
    }

    private LinearLayout row() {
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setGravity(Gravity.CENTER_VERTICAL);
        return row;
    }

    private View brand(boolean compact) {
        LinearLayout brand = row();
        ImageView mark = new ImageView(this);
        mark.setImageResource(R.drawable.ripo_team_logo);
        mark.setScaleType(ImageView.ScaleType.CENTER_CROP);
        mark.setClipToOutline(true);
        mark.setBackground(rounded(Color.WHITE, 100));
        mark.setOutlineProvider(ViewOutlineProvider.BACKGROUND);
        brand.addView(mark, new LinearLayout.LayoutParams(dp(compact ? 44 : 68), dp(compact ? 44 : 68)));
        LinearLayout words = new LinearLayout(this);
        words.setOrientation(LinearLayout.VERTICAL);
        words.setPadding(dp(12), 0, 0, 0);
        TextView name = label("Ripo Team", compact ? 18 : 28, text);
        name.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        words.addView(name);
        words.addView(label("Launcher", compact ? 12 : 15, muted));
        brand.addView(words);
        return brand;
    }

    private EditText field(String hint, boolean password) {
        EditText field = new EditText(this);
        field.setHint(hint);
        field.setTextColor(text);
        field.setHintTextColor(muted);
        field.setSingleLine(true);
        field.setPadding(dp(16), 0, dp(16), 0);
        field.setBackground(rounded(Color.rgb(242, 245, 249), 16));
        field.setInputType(password ? InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD : InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS);
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(54));
        params.setMargins(0, dp(7), 0, dp(7));
        field.setLayoutParams(params);
        return field;
    }

    private Button primaryButton(String title) {
        Button button = new Button(this);
        button.setText(title);
        button.setTextColor(Color.WHITE);
        button.setAllCaps(false);
        button.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        button.setBackground(rounded(blue, 100));
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(52));
        params.setMargins(0, dp(8), 0, dp(4));
        button.setLayoutParams(params);
        return button;
    }

    private Button secondaryButton(String title) {
        Button button = new Button(this);
        button.setText(title);
        button.setTextColor(text);
        button.setAllCaps(false);
        button.setBackground(rounded(Color.rgb(238, 242, 247), 100));
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, dp(50));
        params.setMargins(0, dp(5), 0, dp(5));
        button.setLayoutParams(params);
        return button;
    }

    private TextView label(String value, int size, int color) {
        TextView label = new TextView(this);
        label.setText(value);
        label.setTextSize(size);
        label.setTextColor(color);
        return label;
    }

    private GradientDrawable rounded(int color, int radius) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(color);
        drawable.setCornerRadius(dp(radius));
        return drawable;
    }

    private GradientDrawable roundedGradient(int start, int end, int radius) {
        GradientDrawable drawable = new GradientDrawable(GradientDrawable.Orientation.TL_BR, new int[]{start, end});
        drawable.setCornerRadius(dp(radius));
        return drawable;
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private static String read(InputStream stream) throws Exception {
        if (stream == null) return "{}";
        StringBuilder result = new StringBuilder();
        try (BufferedReader reader = new BufferedReader(new InputStreamReader(stream, StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) result.append(line);
        }
        return result.toString();
    }
}
