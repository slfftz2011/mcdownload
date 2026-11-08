namespace MCDownloader.Services;

public class XamlHomepageGenerator
{
    public string Build()
    {
        // 这是一个最小可用的 PCL2 自定义主页模板
        // 后续把版本列表动态渲染成多个 MyCard / MyButton
        return @"<StackPanel xmlns=""http://schemas.microsoft.com/winfx/2006/xaml/presentation""
                    xmlns:local=""clr-namespace:PCL;assembly=PCL"">
    <local:MyCard Title=""MCDownloader"" CanSwap=""False"" Margin=""0,0,0,12"">
        <StackPanel Margin=""8,35,8,15"">
            <TextBlock Text=""下载器正在运行"" FontSize=""14"" Margin=""0,0,0,8""/>
            <TextBlock Text=""PCL 主页服务已就绪"" Foreground=""Gray"" FontSize=""12""/>
        </StackPanel>
    </local:MyCard>
</StackPanel>";
    }
}