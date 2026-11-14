using System.Text;
using LLama;
using LLama.Common;
using LLama.Native;
using LLama.Sampling;

namespace OcrBench;

/// <summary>
/// A small local language model (a GGUF file, run on the CPU by llama.cpp through LLamaSharp)
/// asked to undo text-recognition mix-ups in a code. Greedy, so the same input always gives
/// the same answer; the prompt is the ChatML format both Qwen2.5 and SmolLM2 were tuned on.
/// </summary>
public sealed class LlmCorrector : ICorrector, IDisposable
{
    private const int MaxTokens = 32;

    private const string System =
        "You correct text-recognition errors in part numbers and drawing numbers. " +
        "Recognition confuses look-alike characters: 0 and O, 1 and I and l, 5 and S, 8 and B, 2 and Z, 6 and G. " +
        "In these codes, letters and digits keep to groups: a run of digits stays digits, a run of letters stays letters, and the word Rev stays Rev. " +
        "Reply with the corrected code only, nothing else.";

    // A few worked examples, none of them from the benchmark's codes.
    private static readonly (string Read, string Fixed)[] Examples =
    [
        ("AB-C-O7O4", "AB-C-0704"),
        ("X1O-OI5-B8", "X10-OI5-B8"),
        ("4Z1B-SDK-I7Q", "4218-SDK-17Q"),
        ("QM2O45_RevB", "QM2045_RevB"),
    ];

    private readonly LLamaWeights _weights;
    private readonly StatelessExecutor _executor;
    private readonly InferenceParams _inference;

    /// <summary>Puts every layer on the GPU; the largest model tried is about 1 GB.</summary>
    private const int AllLayers = 999;

    /// <param name="gpu">Run on the GPU chosen with <see cref="Device"/>, else on the CPU.</param>
    public LlmCorrector(string modelPath, bool gpu)
    {
        Name = Path.GetFileNameWithoutExtension(modelPath);
        var parameters = new ModelParams(modelPath)
        {
            ContextSize = 1024,
            GpuLayerCount = gpu ? AllLayers : 0,
        };
        _weights = LLamaWeights.LoadFromFile(parameters);
        _executor = new StatelessExecutor(_weights, parameters);
        _inference = new InferenceParams
        {
            MaxTokens = MaxTokens,
            AntiPrompts = ["<|im_end|>", "\n"],
            SamplingPipeline = new DefaultSamplingPipeline { Temperature = 0f },
        };
    }

    public string Name { get; }

    // The last answer only, so the index-checked variant that runs next on the same crop asks the model once
    // (its time is added in Program), while every crop's first ask is timed in full.
    private (string Read, string Answer)? _last;

    public async Task<string> CorrectAsync(string read)
    {
        if (read.Length == 0)
        {
            return read;
        }
        if (_last is var (lastRead, lastAnswer) && lastRead == read)
        {
            _last = null;
            return lastAnswer;
        }
        string answer = await AskAsync(read);
        _last = (read, answer);
        return answer;
    }

    private async Task<string> AskAsync(string read)
    {
        var answer = new StringBuilder();
        await foreach (string piece in _executor.InferAsync(Prompt(read), _inference))
        {
            answer.Append(piece);
        }
        string text = answer.ToString().Replace("<|im_end|>", "").Trim();
        // A model that wanders off gets no say: the read is searched as it was.
        return text.Length == 0 || text.Contains(' ') || Math.Abs(text.Length - read.Length) > 2 ? read : text;
    }

    public void Dispose() => _weights.Dispose();

    /// <summary>
    /// Chooses llama.cpp's backend before any model loads: "cpu", "cuda" or "vulkan". There is no
    /// fallback, so a GPU that cannot be used fails the run instead of quietly measuring the CPU.
    /// </summary>
    public static void Device(string device)
    {
        NativeLibraryConfig.All
            .WithCuda(device == "cuda")
            .WithVulkan(device == "vulkan")
            .WithAutoFallback(false);
    }

    /// <summary>Whether the loaded backend can run layers on a GPU.</summary>
    public static bool GpuLoaded => LLama.Native.NativeApi.llama_supports_gpu_offload();

    private static string Prompt(string read)
    {
        var prompt = new StringBuilder();
        prompt.Append("<|im_start|>system\n").Append(System).Append("<|im_end|>\n");
        foreach (var (example, fixedCode) in Examples)
        {
            prompt.Append("<|im_start|>user\n").Append(example).Append("<|im_end|>\n");
            prompt.Append("<|im_start|>assistant\n").Append(fixedCode).Append("<|im_end|>\n");
        }
        prompt.Append("<|im_start|>user\n").Append(read).Append("<|im_end|>\n<|im_start|>assistant\n");
        return prompt.ToString();
    }
}
