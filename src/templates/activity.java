package __PACKAGE__;

import android.app.Activity;
import android.os.Bundle;
import android.util.Log;
import android.widget.TextView;

public class MainActivity extends Activity {
    @Override public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        TextView title = new TextView(this);
        title.setText(R.string.app_name);
        title.setTextSize(28f);
        title.setGravity(android.view.Gravity.CENTER);
        setContentView(title);
        Log.i("WexRun", "WEX_RUN_OK runId=" + getIntent().getStringExtra("wex.run_id"));
    }
    @Override public void onResume() {
        super.onResume();
        Log.i("WexRun", "WEX_ACTIVITY_RESUMED runId=" + getIntent().getStringExtra("wex.run_id"));
    }
}
