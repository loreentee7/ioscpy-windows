using System;
using System.Text;
using System.Security.Cryptography;
using System.Windows.Forms;

public static class AskPass {
    [STAThread]
    public static int Main(string[] args) {
        string prompt = args.Length > 0 ? args[0] : "";
        if (prompt.IndexOf("password", StringComparison.OrdinalIgnoreCase) >= 0) {
            try {
                byte[] data = Convert.FromBase64String(Environment.GetEnvironmentVariable("IOSCPY_CREDENTIAL") ?? "");
                byte[] clear = ProtectedData.Unprotect(data, null, DataProtectionScope.CurrentUser);
                Console.WriteLine(Encoding.UTF8.GetString(clear));
                Array.Clear(clear, 0, clear.Length);
                return 0;
            } catch { return 1; }
        }
        if (prompt.IndexOf("yes/no", StringComparison.OrdinalIgnoreCase) >= 0) {
            var answer = MessageBox.Show(prompt + "\n\nComprueba la huella con el iPhone antes de aceptar.", "Verificar iPhone", MessageBoxButtons.YesNo, MessageBoxIcon.Warning);
            Console.WriteLine(answer == DialogResult.Yes ? "yes" : "no");
            return 0;
        }
        return 1;
    }
}
