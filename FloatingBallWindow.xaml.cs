using System.Windows;
using System.Windows.Input;

namespace MCDownloader;

public partial class FloatingBallWindow : Window
{
    private readonly MainWindow _main;

    public FloatingBallWindow(MainWindow main)
    {
        InitializeComponent();
        _main = main;
        MouseDoubleClick += (_, _) => { _main.ReturnFromBall(); };
    }

    private void Border_MouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (e.ButtonState == MouseButtonState.Pressed)
            DragMove();
    }

    public void UpdateProgress(double percent, string speed)
    {
        PercentText.Text = $"{percent:0}%";
        SpeedText.Text = speed;
    }
}